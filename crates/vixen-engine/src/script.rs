//! `deno_core` runtime — the script execution boundary.
//!
//! The public Vixen-facing seam stays small (`JsRuntime`, `JsValue`, eval
//! methods), but the implementation uses `deno_core`/V8 directly per ADR-014.
//! Host surfaces are installed from focused bootstrap modules before the caller's
//! script runs. Each evaluation uses a fresh JS runtime today to preserve the
//! earlier Phase 2 semantics: `--eval` sees a clean global, and `document` only
//! exists for `evaluate_with_page`.

#![forbid(unsafe_code)]

use crate::engine_error::{EngineError, codes};
use crate::page::Page;

mod cssom;
mod dom;
mod encoding;
mod runtime;

/// Vixen's JavaScript runtime seam, backed by `deno_core`/V8.
pub struct JsRuntime;

/// A safe subset of a JS value returned across the runtime boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum JsValue {
    Int32(i32),
    Number(f64),
    String(String),
    Bool(bool),
    Null,
    Undefined,
    /// Any non-scalar (object, symbol, etc.) — not introspected here.
    Object,
}

impl JsValue {
    /// The JS string representation used by `--eval` output (matches the
    /// scalar conversions; objects render as `"[object]"`).
    pub fn to_display(&self) -> String {
        match self {
            JsValue::Int32(n) => n.to_string(),
            JsValue::Number(n) => format_number(*n),
            JsValue::String(s) => s.clone(),
            JsValue::Bool(b) => b.to_string(),
            JsValue::Null => "null".to_owned(),
            JsValue::Undefined => "undefined".to_owned(),
            JsValue::Object => "[object]".to_owned(),
        }
    }
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

impl JsRuntime {
    /// Initialise the V8 platform through `deno_core`.
    pub fn new() -> Result<Self, EngineError> {
        let _ = runtime::new_deno_runtime(None)?;
        Ok(Self)
    }

    /// Evaluate `src` in a fresh JS global and return the result.
    pub fn evaluate(&mut self, src: &str) -> Result<JsValue, EngineError> {
        self.evaluate_with_page_context(src, None)
    }

    /// Evaluate `src` in a fresh JS global with read-only DOM host objects
    /// projected from `page`.
    pub fn evaluate_with_page(&mut self, src: &str, page: &Page) -> Result<JsValue, EngineError> {
        self.evaluate_with_page_context(src, Some(page))
    }

    fn evaluate_with_page_context(
        &mut self,
        src: &str,
        page: Option<&Page>,
    ) -> Result<JsValue, EngineError> {
        let mut runtime = runtime::new_deno_runtime(page)?;

        let result = runtime
            .execute_script("inline.js", src.to_owned())
            .map_err(|_| {
                EngineError::script(codes::SCRIPT_EVAL, "script evaluation raised an exception")
            })?;
        runtime::js_value_from_global(&mut runtime, result)
    }
}

impl Default for JsRuntime {
    fn default() -> Self {
        Self::new().expect("deno_core runtime must initialise")
    }
}

/// Evaluate `src` as an **inline script** only if `csp` permits it
/// (docs/SPEC.md "CSP enforcement points", docs/PLAN.md Phase 7 step 1).
/// This is the trust boundary between untrusted page script and the engine:
/// CSP is checked *before* script execution. Fail closed: no CSP ⇒ allow
/// (no restriction); a CSP that doesn't explicitly permit the inline script
/// (via `'unsafe-inline'`, a matching nonce, or a matching sha256 hash) ⇒
/// [`EngineError`] with the stable [`codes::SCRIPT_CSP_BLOCKED`] code.
///
/// `origin` is the document origin (`'self'` resolves against it).
pub fn evaluate_inline_script(
    rt: &mut JsRuntime,
    csp: Option<&vixen_net::csp::ContentSecurityPolicy>,
    origin: &vixen_net::Origin,
    src: &str,
    nonce: Option<&str>,
) -> Result<JsValue, EngineError> {
    if let Some(policy) = csp
        && !policy.allows_inline_script(origin, Some(src), nonce)
    {
        return Err(EngineError::script(
            codes::SCRIPT_CSP_BLOCKED,
            "inline script blocked by Content-Security-Policy",
        ));
    }
    rt.evaluate(src)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_runs() {
        let mut rt = JsRuntime::new().expect("engine init");

        // Phase 2 gate (docs/PLAN.md): `--eval '1+2'` returns 3.
        assert_eq!(rt.evaluate("1 + 2").unwrap(), JsValue::Int32(3));
        assert_eq!(rt.evaluate("40 + 2").unwrap(), JsValue::Int32(42));

        // Scalar conversions.
        assert_eq!(
            rt.evaluate("0.1 + 0.2").unwrap(),
            JsValue::Number(0.1 + 0.2)
        );
        assert_eq!(rt.evaluate("1 < 2").unwrap(), JsValue::Bool(true));
        assert_eq!(
            rt.evaluate("'hi'").unwrap(),
            JsValue::String("hi".to_owned())
        );
        assert_eq!(rt.evaluate("null").unwrap(), JsValue::Null);
        assert_eq!(rt.evaluate("undefined").unwrap(), JsValue::Undefined);
        assert!(matches!(rt.evaluate("({})").unwrap(), JsValue::Object));

        // Phase 6 pilot: Encoding API constructors live in the deno_core global.
        assert_eq!(
            rt.evaluate("new TextEncoder().encoding").unwrap(),
            JsValue::String("utf-8".to_owned())
        );
        assert_eq!(
            rt.evaluate("new TextEncoder().encode('é').length").unwrap(),
            JsValue::Int32(2)
        );
        assert_eq!(
            rt.evaluate("new TextEncoder().encode('A')[0]").unwrap(),
            JsValue::Int32(65)
        );
        assert_eq!(
            rt.evaluate("new TextEncoder().encodeInto('aé', new Uint8Array(3)).read")
                .unwrap(),
            JsValue::Int32(2)
        );
        assert_eq!(
            rt.evaluate("new TextEncoder().encodeInto('aé', new Uint8Array(3)).written")
                .unwrap(),
            JsValue::Int32(3)
        );
        assert_eq!(
            rt.evaluate("new TextDecoder().decode([65,13,10,66])")
                .unwrap(),
            JsValue::String("A\nB".to_owned())
        );
        assert_eq!(
            rt.evaluate("new TextDecoder('UTF-8', { fatal: true }).fatal")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("new TextDecoder('utf-8', { ignoreBOM: true }).ignoreBOM")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("new TextDecoder('utf-8', { fatal: true }).decode([255])")
                .unwrap_err()
                .code(),
            codes::SCRIPT_EVAL
        );
        assert_eq!(
            rt.evaluate("new TextDecoder('windows-1252')")
                .unwrap_err()
                .code(),
            codes::SCRIPT_EVAL
        );

        // Phase 6 DOM host-object backbone: page DOM data is projected into the
        // deno_core global as `document` / read-only `Element` / DOMTokenList /
        // DOMStringMap objects.
        let page = Page::from_html(
            "file:///dom-host.html",
            "<html><head><title>DOM host</title><style>#lead { color: blue; font-size: 20px !important; --Token: A:B; } p { margin-left: 4px; }</style><link id='theme' rel='stylesheet alternate'></head><body><p id='lead' class='note note callout' data-role='copy' data-author-name='ada' style='font-size: 18px; margin-left: 10px'>Hello <b>world</b></p><iframe id='frame' sandbox='allow-scripts allow-same-origin'></iframe></body></html>",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("typeof document").unwrap(),
            JsValue::String("undefined".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.title", &page).unwrap(),
            JsValue::String("DOM host".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.body.textContent", &page)
                .unwrap(),
            JsValue::String("Hello world".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.body === document.querySelector('body')", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').textContent", &page)
                .unwrap(),
            JsValue::String("Hello world".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').tagName", &page)
                .unwrap(),
            JsValue::String("P".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').className", &page)
                .unwrap(),
            JsValue::String("note note callout".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.getElementById('lead').getAttribute('data-role')",
                &page
            )
            .unwrap(),
            JsValue::String("copy".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.getElementById('lead').hasAttribute('DATA-ROLE')",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').ownerDocument === document",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelectorAll('p').length", &page)
                .unwrap(),
            JsValue::Int32(1)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelectorAll('.note').length", &page)
                .unwrap(),
            JsValue::Int32(1)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#missing') === null", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('.callout') === document.getElementById('lead')",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('p').matches('.note')", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('p.note')", &page)
                .unwrap_err()
                .code(),
            codes::SCRIPT_EVAL
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').classList.length", &page)
                .unwrap(),
            JsValue::Int32(2)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').classList.item(1)", &page)
                .unwrap(),
            JsValue::String("callout".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').classList.contains('note')",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').classList.value", &page)
                .unwrap(),
            JsValue::String("note callout".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#theme').relList.contains('alternate')",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#frame').sandbox.length", &page)
                .unwrap(),
            JsValue::Int32(2)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#frame').sandbox.item(0)", &page)
                .unwrap(),
            JsValue::String("allow-scripts".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').dataset.role", &page)
                .unwrap(),
            JsValue::String("copy".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').dataset['authorName']",
                &page
            )
            .unwrap(),
            JsValue::String("ada".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').dataset.missing", &page)
                .unwrap(),
            JsValue::Undefined
        );
        assert_eq!(
            rt.evaluate_with_page("CSS.supports('display', 'grid')", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("CSS.supports('(unknown-prop: yes)')", &page)
                .unwrap(),
            JsValue::Bool(false)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "getComputedStyle(document.querySelector('#lead')).color",
                &page
            )
            .unwrap(),
            JsValue::String("blue".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "window.getComputedStyle(document.querySelector('#lead')).fontSize",
                &page
            )
            .unwrap(),
            JsValue::String("20px".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "getComputedStyle(document.querySelector('#lead')).getPropertyValue('margin-left')",
                &page
            )
            .unwrap(),
            JsValue::String("10px".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "getComputedStyle(document.querySelector('#lead')).getPropertyValue('--Token')",
                &page
            )
            .unwrap(),
            JsValue::String("A:B".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets.length", &page)
                .unwrap(),
            JsValue::Int32(1)
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].cssRules.length", &page)
                .unwrap(),
            JsValue::Int32(2)
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].href === null", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].cssRules[0].selectorText", &page)
                .unwrap(),
            JsValue::String("#lead".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].cssRules[0].style.length", &page)
                .unwrap(),
            JsValue::Int32(3)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.styleSheets[0].cssRules[0].style.getPropertyValue('font-size')",
                &page
            )
            .unwrap(),
            JsValue::String("20px".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].cssRules[1].style[0]", &page)
                .unwrap(),
            JsValue::String("margin-left".to_owned())
        );

        let unicode_page = Page::from_html(
            "file:///dom-host-unicode.html",
            "<html><head><title>é—😀</title></head><body><p id='lead' data-emoji='é'>body é—😀</p></body></html>",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate_with_page("document.title", &unicode_page)
                .unwrap(),
            JsValue::String("é—😀".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').textContent", &unicode_page)
                .unwrap(),
            JsValue::String("body é—😀".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').dataset.emoji",
                &unicode_page
            )
            .unwrap(),
            JsValue::String("é".to_owned())
        );

        // Errors surface a stable code.
        assert_eq!(
            rt.evaluate("throw new Error('boom')").unwrap_err().code(),
            codes::SCRIPT_EVAL
        );

        // Display stringification matches JS scalars.
        assert_eq!(JsValue::Int32(3).to_display(), "3");
        assert_eq!(JsValue::Number(2.5).to_display(), "2.5");
        assert_eq!(JsValue::Number(4.0).to_display(), "4");
        assert_eq!(JsValue::String("x".into()).to_display(), "x");

        // CSP enforcement at the script boundary (Phase 7 step 1).
        let origin = vixen_net::Origin::from_url(&url::Url::parse("https://example.com").unwrap());
        // A strict CSP blocks inline scripts (fail closed).
        let mut strict = vixen_net::csp::ContentSecurityPolicy::new();
        strict.add_header("default-src 'self'");
        let err = evaluate_inline_script(&mut rt, Some(&strict), &origin, "1+2", None).unwrap_err();
        assert_eq!(err.code(), codes::SCRIPT_CSP_BLOCKED);
        // 'unsafe-inline' permits it.
        let mut allow = vixen_net::csp::ContentSecurityPolicy::new();
        allow.add_header("script-src 'unsafe-inline'");
        assert_eq!(
            evaluate_inline_script(&mut rt, Some(&allow), &origin, "1+2", None).unwrap(),
            JsValue::Int32(3)
        );
        // No CSP ⇒ no restriction.
        assert_eq!(
            evaluate_inline_script(&mut rt, None, &origin, "1+2", None).unwrap(),
            JsValue::Int32(3)
        );
    }
}
