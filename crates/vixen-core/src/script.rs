//! SpiderMonkey runtime — the script execution boundary
//! (docs/SPEC.md, docs/ARCHITECTURE.md "Trust boundaries"; ADR-004/ADR-005).
//!
//! `unsafe` is confined to this module: the SpiderMonkey FFI (`mozjs`) is C,
//! and GC rooting is enforced via mozjs's `rooted!` macro — no naked handles
//! (docs/PLAN.md Phase 2 step 4). Phase 2 implements the runtime + `evaluate`;
//! host hooks (`console.log`, `fetch`→`vixen-net`, `document.title`) and
//! per-origin compartments land with the DOM (Phase 6).
//!
//! Engine lifetime: SpiderMonkey is a process-singleton — `JS_Init`/`JS_ShutDown`
//! run once per process. [`JsRuntime`] therefore owns the `JSEngine` (so a
//! single runtime + clean shutdown works in any one process). A process-global
//! engine shared by many runtimes is the natural follow-up (Phase 6, with host
//! hooks); it needs a `Sync` wrapper around `JSEngine` and explicit shutdown.
//! `vixen-headless` creates one runtime per invocation, which is all the
//! Phase 2 gate needs.

#![allow(unsafe_code)] // SpiderMonkey FFI boundary.
#![allow(
    non_upper_case_globals,
    non_snake_case,
    non_camel_case_types,
    improper_ctypes
)]

use std::ffi::CString;
use std::ptr;

use mozjs::conversions::jsstr_to_string;
use mozjs::jsapi::OnNewGlobalHookOption;
use mozjs::jsval::UndefinedValue;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{
    CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS, evaluate_script,
};

use crate::engine_error::{EngineError, codes};

/// A SpiderMonkey JS runtime. Owns the engine + `mozjs::rust::Runtime`.
/// Create one per process (SpiderMonkey is a process-singleton).
pub struct JsRuntime {
    // Field order matters: `rt` must drop before `_engine`.
    rt: Runtime,
    _engine: JSEngine,
}

/// A safe subset of a JS value returned across the FFI boundary.
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
    /// Initialise SpiderMonkey. At most one `JsRuntime` may exist per process.
    pub fn new() -> Result<Self, EngineError> {
        let engine = JSEngine::init().map_err(|_| EngineError::Other {
            code: codes::SCRIPT_OOM,
            message: "SpiderMonkey engine initialisation failed".into(),
        })?;
        let rt = Runtime::new(engine.handle());
        Ok(Self {
            rt,
            _engine: engine,
        })
    }

    /// Evaluate `src` in a fresh simple global and return the result.
    ///
    /// Per docs/PLAN.md Phase 2 the v1 target is one default compartment;
    /// `compartment_for_origin(&Origin)` lands with the host bindings
    /// (Phase 6). Correctness over efficiency for now: the gate
    /// (`--eval '1+2'` → `3`) needs only a working eval.
    pub fn evaluate(&mut self, src: &str) -> Result<JsValue, EngineError> {
        let options = RealmOptions::default();
        let filename: CString = c"inline.js".to_owned();
        // CompileOptionsWrapper borrows the context immutably (short-lived);
        // take it before the mutable `cx` borrow below.
        let compile = CompileOptionsWrapper::new(self.rt.cx_no_gc(), filename, 1);

        let cx = self.rt.cx();
        rooted!(&in(cx) let global = unsafe {
            JS_NewGlobalObject(
                cx,
                &SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                OnNewGlobalHookOption::FireOnNewGlobalHook,
                &*options,
            )
        });

        rooted!(&in(cx) let mut rval = UndefinedValue());
        let ok = evaluate_script(cx, global.handle(), src, rval.handle_mut(), compile);

        if ok.is_err() {
            // SpiderMonkey reports the exception to the runtime's error hook;
            // vixen surfaces a stable code. Full message extraction
            // (JS_GetPendingException) lands with the host hooks (Phase 6).
            return Err(EngineError::script(
                codes::SCRIPT_EVAL,
                "script evaluation raised an exception",
            ));
        }

        // Read the rooted Value into the safe JsValue subset. The Value type
        // is private to mozjs, so operate on it by inference — no annotation.
        let v = rval.get();
        if v.is_undefined() {
            Ok(JsValue::Undefined)
        } else if v.is_null() {
            Ok(JsValue::Null)
        } else if v.is_boolean() {
            Ok(JsValue::Bool(v.to_boolean()))
        } else if v.is_int32() {
            Ok(JsValue::Int32(v.to_int32()))
        } else if v.is_double() {
            Ok(JsValue::Number(v.to_double()))
        } else if v.is_string() {
            // SAFETY: `to_string()` yields a valid `JSString*` rooted for the
            // current stack frame; `jsstr_to_string` copies it into a Rust
            // `String` before we return.
            unsafe {
                let jsstr = v.to_string();
                match ptr::NonNull::new(jsstr) {
                    Some(s) => Ok(JsValue::String(jsstr_to_string(cx, s))),
                    None => Ok(JsValue::Null),
                }
            }
        } else {
            Ok(JsValue::Object)
        }
    }
}

impl Default for JsRuntime {
    fn default() -> Self {
        Self::new().expect("SpiderMonkey engine must initialise")
    }
}

/// Evaluate `src` as an **inline script** only if `csp` permits it
/// (docs/SPEC.md "CSP enforcement points", docs/PLAN.md Phase 7 step 1).
/// This is the trust boundary between untrusted page script and the engine:
/// CSP is checked *before* `EvaluateScript`. Fail closed: no CSP ⇒ allow
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

    // SpiderMonkey is a process-singleton (one JS_Init/JS_ShutDown per
    // process), so all eval assertions share a single JsRuntime in one test.
    // (mozjs's own tests achieve one-engine-per-process by placing each test
    // in its own `tests/*.rs` binary.)
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
