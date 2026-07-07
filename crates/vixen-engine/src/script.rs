//! SpiderMonkey runtime — the script execution boundary
//! (docs/SPEC.md, docs/ARCHITECTURE.md "Trust boundaries"; ADR-004/ADR-005).
//!
//! `unsafe` is confined to this module: the SpiderMonkey FFI (`mozjs`) is C,
//! and GC rooting is enforced via mozjs's `rooted!` macro — no naked handles
//! (docs/PLAN.md Phase 2 step 4). Phase 2 implements the runtime + `evaluate`;
//! host hooks (`console.log`, `fetch`→`vixen-net`) and broader per-origin
//! compartments land with the DOM (Phase 6). The first focused `document` /
//! `Element` snapshot objects now install at this boundary.
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
use std::fmt::Write as _;
use std::ptr;
use std::ptr::NonNull;

use mozjs::context::{JSContext, RawJSContext};
use mozjs::conversions::jsstr_to_string;
use mozjs::conversions::{ConversionResult, FromJSValConvertible, ToJSValConvertible};
use mozjs::jsapi::{CallArgs, JS_ReportErrorUTF8, JSObject, OnNewGlobalHookOption, Value};
use mozjs::jsval::{Int32Value, ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{JS_DefineFunction, JS_NewGlobalObject, JS_NewPlainObject};
use mozjs::rust::{
    CompileOptionsWrapper, Handle, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS,
    evaluate_script,
};
use mozjs::typedarray::{CreateWith, Uint8Array};

use crate::engine_error::{EngineError, codes};
use crate::page::Page;
use crate::text_codec::{TextDecoder, TextEncoder};

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
        self.evaluate_with_page_context(src, None)
    }

    /// Evaluate `src` in a fresh simple global with read-only DOM host objects
    /// projected from `page`.
    ///
    /// This is the Phase 6 DOM-host backbone: the loaded page crosses into the
    /// script boundary as a deterministic snapshot, then `document` /
    /// `Element` methods execute inside SpiderMonkey rather than the older
    /// headless string-smoke matcher.
    pub fn evaluate_with_page(&mut self, src: &str, page: &Page) -> Result<JsValue, EngineError> {
        self.evaluate_with_page_context(src, Some(page))
    }

    fn evaluate_with_page_context(
        &mut self,
        src: &str,
        page: Option<&Page>,
    ) -> Result<JsValue, EngineError> {
        let options = RealmOptions::default();
        let filename: CString = c"inline.js".to_owned();
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
        let mut realm = AutoRealm::new_from_handle(cx, global.handle());
        let (global_handle, realm) = realm.global_and_reborrow();
        let cx = realm;

        install_encoding_api(cx, global_handle)?;
        if let Some(page) = page {
            install_dom_api(cx, global_handle, page)?;
        }

        let compile = CompileOptionsWrapper::new(cx, filename, 1);

        rooted!(&in(cx) let mut rval = UndefinedValue());
        let ok = evaluate_script(cx, global_handle, src, rval.handle_mut(), compile);

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

fn install_dom_api(
    cx: &mut JSContext,
    global: mozjs::rust::HandleObject,
    page: &Page,
) -> Result<(), EngineError> {
    let bootstrap = dom_api_bootstrap(page).map_err(|err| {
        EngineError::script(
            codes::SCRIPT_EVAL,
            format!("failed to build DOM host snapshot: {err}"),
        )
    })?;
    rooted!(&in(cx) let mut rval = UndefinedValue());
    let compile = CompileOptionsWrapper::new(cx, c"vixen-dom-api.js".to_owned(), 1);
    evaluate_script(cx, global, &bootstrap, rval.handle_mut(), compile).map_err(|_| {
        EngineError::script(codes::SCRIPT_EVAL, "failed to install DOM host objects")
    })?;
    Ok(())
}

fn dom_api_bootstrap(page: &Page) -> Result<String, String> {
    let mut script = String::new();
    script.push_str("(() => {\nconst data = ");
    script.push_str(&dom_snapshot_literal(page)?);
    script.push_str(";\n");
    script.push_str(DOM_API_BOOTSTRAP_BODY);
    Ok(script)
}

fn dom_snapshot_literal(page: &Page) -> Result<String, String> {
    let elements = page.query_selector_all("*")?;
    let mut out = String::new();
    out.push_str("{title:");
    out.push_str(&js_string_literal(
        &page.document().title().unwrap_or_default(),
    ));
    out.push_str(",url:");
    out.push_str(&js_string_literal(page.url()));
    out.push_str(",elements:[");
    for (index, info) in elements.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{nodeId:");
        out.push_str(&info.node_id.to_string());
        out.push_str(",tag:");
        out.push_str(&js_string_literal(&info.tag));
        out.push_str(",id:");
        if let Some(id) = &info.id {
            out.push_str(&js_string_literal(id));
        } else {
            out.push_str("null");
        }
        out.push_str(",classes:");
        write_js_string_array(&mut out, &info.classes);
        out.push_str(",attrs:[");
        for (attr_index, (name, value)) in info.attributes.iter().enumerate() {
            if attr_index > 0 {
                out.push(',');
            }
            out.push('[');
            out.push_str(&js_string_literal(name));
            out.push(',');
            out.push_str(&js_string_literal(value));
            out.push(']');
        }
        out.push_str("],textContent:");
        let text_content = page
            .document()
            .element_text_content(info.node_id)
            .unwrap_or_else(|| info.text.clone());
        out.push_str(&js_string_literal(&text_content));
        out.push('}');
    }
    out.push_str("]}");
    Ok(out)
}

fn write_js_string_array(out: &mut String, values: &[String]) {
    out.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&js_string_literal(value));
    }
    out.push(']');
}

fn js_string_literal(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    out.push('"');
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if c < ' ' => {
                write!(&mut out, "\\u{:04X}", c as u32).expect("write to String cannot fail");
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

const DOM_API_BOOTSTRAP_BODY: &str = r#"
  const byId = new Map();
  for (const record of data.elements) {
    if (record.id !== null && !byId.has(record.id)) {
      byId.set(record.id, record);
    }
  }

  function unsupportedSelector(selector) {
    throw new TypeError('Vixen DOM host currently supports simple #id, .class, tag, and * selectors: ' + selector);
  }

  function parseSimpleSelector(selector) {
    const raw = String(selector).trim();
    if (raw === '*') return { kind: 'all', value: '*' };
    if (/^#[A-Za-z_][A-Za-z0-9_-]*$/.test(raw)) return { kind: 'id', value: raw.slice(1) };
    if (/^\.[A-Za-z_][A-Za-z0-9_-]*$/.test(raw)) return { kind: 'class', value: raw.slice(1) };
    if (/^[A-Za-z][A-Za-z0-9_-]*$/.test(raw)) return { kind: 'tag', value: raw.toLowerCase() };
    unsupportedSelector(raw);
  }

  function recordMatches(record, parsed) {
    switch (parsed.kind) {
      case 'all': return true;
      case 'id': return record.id === parsed.value;
      case 'class': return record.classes.includes(parsed.value);
      case 'tag': return record.tag.toLowerCase() === parsed.value;
      default: return false;
    }
  }

  function findAll(selector) {
    const parsed = parseSimpleSelector(selector);
    return data.elements.filter((record) => recordMatches(record, parsed));
  }

  function attrPair(record, name) {
    const raw = String(name);
    const lower = raw.toLowerCase();
    return record.attrs.find(([attr]) => attr === raw || attr === lower) || null;
  }

  function wrapElement(record) {
    if (record == null) return null;
    if (!record.__vixenObject) {
      Object.defineProperty(record, '__vixenObject', {
        value: new VixenElement(record),
        configurable: false,
      });
    }
    return record.__vixenObject;
  }

  class VixenElement {
    constructor(record) {
      Object.defineProperty(this, '__vixenRecord', {
        value: record,
        enumerable: false,
      });
    }
    get id() { return this.__vixenRecord.id || ''; }
    get className() { return this.__vixenRecord.classes.join(' '); }
    get tagName() { return this.__vixenRecord.tag.toUpperCase(); }
    get nodeName() { return this.tagName; }
    get localName() { return this.__vixenRecord.tag; }
    get nodeType() { return 1; }
    get isConnected() { return true; }
    get ownerDocument() { return vixenDocument; }
    get textContent() { return this.__vixenRecord.textContent; }
    get innerText() { return this.__vixenRecord.textContent; }
    getAttribute(name) {
      const pair = attrPair(this.__vixenRecord, name);
      return pair ? pair[1] : null;
    }
    hasAttribute(name) { return attrPair(this.__vixenRecord, name) !== null; }
    matches(selector) { return recordMatches(this.__vixenRecord, parseSimpleSelector(selector)); }
  }

  const vixenDocument = {};
  Object.defineProperties(vixenDocument, {
    title: { get() { return data.title; }, enumerable: true, configurable: true },
    URL: { get() { return data.url; }, enumerable: true, configurable: true },
    documentURI: { get() { return data.url; }, enumerable: true, configurable: true },
    readyState: { get() { return 'complete'; }, enumerable: true, configurable: true },
    body: {
      get() { return wrapElement(data.elements.find((record) => record.tag.toLowerCase() === 'body') || null); },
      enumerable: true,
      configurable: true,
    },
    documentElement: {
      get() { return wrapElement(data.elements.find((record) => record.tag.toLowerCase() === 'html') || null); },
      enumerable: true,
      configurable: true,
    },
  });
  Object.defineProperties(vixenDocument, {
    querySelector: {
      value(selector) { return wrapElement(findAll(selector)[0] || null); },
      enumerable: true,
      configurable: true,
    },
    querySelectorAll: {
      value(selector) { return findAll(selector).map(wrapElement); },
      enumerable: true,
      configurable: true,
    },
    getElementById: {
      value(id) { return wrapElement(byId.get(String(id)) || null); },
      enumerable: true,
      configurable: true,
    },
  });
  Object.defineProperty(globalThis, 'document', {
    value: vixenDocument,
    writable: true,
    configurable: true,
  });
})();
"#;

fn install_encoding_api(
    cx: &mut JSContext,
    global: mozjs::rust::HandleObject,
) -> Result<(), EngineError> {
    unsafe {
        for (name, callback, nargs) in [
            (
                c"__vixen_text_encoder_encode".as_ptr(),
                Some(text_encoder_encode as unsafe extern "C" fn(_, _, _) -> _),
                1,
            ),
            (
                c"__vixen_text_encoder_encode_into".as_ptr(),
                Some(text_encoder_encode_into as unsafe extern "C" fn(_, _, _) -> _),
                2,
            ),
            (
                c"__vixen_text_decoder_validate".as_ptr(),
                Some(text_decoder_validate as unsafe extern "C" fn(_, _, _) -> _),
                3,
            ),
            (
                c"__vixen_text_decoder_decode".as_ptr(),
                Some(text_decoder_decode as unsafe extern "C" fn(_, _, _) -> _),
                4,
            ),
        ] {
            let function = JS_DefineFunction(cx, global, name, callback, nargs, 0);
            if function.is_null() {
                return Err(EngineError::script(
                    codes::SCRIPT_OOM,
                    "failed to install Encoding API native hook",
                ));
            }
        }
    }

    rooted!(&in(cx) let mut rval = UndefinedValue());
    let compile = CompileOptionsWrapper::new(cx, c"vixen-encoding-api.js".to_owned(), 1);
    evaluate_script(
        cx,
        global,
        ENCODING_API_BOOTSTRAP,
        rval.handle_mut(),
        compile,
    )
    .map_err(|_| {
        EngineError::script(
            codes::SCRIPT_EVAL,
            "failed to install Encoding API host constructors",
        )
    })?;
    Ok(())
}

const ENCODING_API_BOOTSTRAP: &str = r#"
(() => {
  const encoderEncode = globalThis.__vixen_text_encoder_encode;
  const encoderEncodeInto = globalThis.__vixen_text_encoder_encode_into;
  const decoderValidate = globalThis.__vixen_text_decoder_validate;
  const decoderDecode = globalThis.__vixen_text_decoder_decode;

  class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(input = '') { return encoderEncode(String(input)); }
    encodeInto(input = '', destination) {
      if (!(destination instanceof Uint8Array)) {
        throw new TypeError('TextEncoder.encodeInto destination must be a Uint8Array');
      }
      return encoderEncodeInto(String(input), destination);
    }
  }

  class TextDecoder {
    constructor(label = 'utf-8', options = {}) {
      const opts = options == null ? {} : Object(options);
      this.__vixenLabel = decoderValidate(String(label), !!opts.fatal, !!opts.ignoreBOM);
      this.__vixenFatal = !!opts.fatal;
      this.__vixenIgnoreBOM = !!opts.ignoreBOM;
    }
    get encoding() { return 'utf-8'; }
    get fatal() { return this.__vixenFatal; }
    get ignoreBOM() { return this.__vixenIgnoreBOM; }
    decode(input = new Uint8Array()) {
      const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
      return decoderDecode(this.__vixenLabel, this.__vixenFatal, this.__vixenIgnoreBOM, bytes);
    }
  }

  Object.defineProperty(globalThis, 'TextEncoder', {
    value: TextEncoder,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, 'TextDecoder', {
    value: TextDecoder,
    writable: true,
    configurable: true,
  });

  delete globalThis.__vixen_text_encoder_encode;
  delete globalThis.__vixen_text_encoder_encode_into;
  delete globalThis.__vixen_text_decoder_validate;
  delete globalThis.__vixen_text_decoder_decode;
})();
"#;

unsafe extern "C" fn text_encoder_encode(
    raw_cx: *mut RawJSContext,
    argc: u32,
    vp: *mut Value,
) -> bool {
    let Some(mut cx) = callback_context(raw_cx) else {
        return false;
    };
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    let input = match arg_string(&mut cx, &args, 0, "TextEncoder.encode input") {
        Some(input) => input,
        None => return false,
    };
    let bytes = TextEncoder.encode(&input);

    rooted!(&in(cx) let mut array = ptr::null_mut::<JSObject>());
    if unsafe { Uint8Array::create(cx.raw_cx(), CreateWith::Slice(&bytes), array.handle_mut()) }
        .is_err()
    {
        report_error(&mut cx, "TextEncoder.encode failed to allocate Uint8Array");
        return false;
    }
    args.rval().set(ObjectValue(array.get()));
    true
}

unsafe extern "C" fn text_encoder_encode_into(
    raw_cx: *mut RawJSContext,
    argc: u32,
    vp: *mut Value,
) -> bool {
    let Some(mut cx) = callback_context(raw_cx) else {
        return false;
    };
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    let input = match arg_string(&mut cx, &args, 0, "TextEncoder.encodeInto input") {
        Some(input) => input,
        None => return false,
    };
    let Some(dest_value) = arg_handle(&args, 1) else {
        report_error(&mut cx, "TextEncoder.encodeInto destination is required");
        return false;
    };
    if !dest_value.get().is_object() {
        report_error(
            &mut cx,
            "TextEncoder.encodeInto destination must be a Uint8Array",
        );
        return false;
    }
    let mut dest = match Uint8Array::from(dest_value.get().to_object()) {
        Ok(dest) => dest,
        Err(()) => {
            report_error(
                &mut cx,
                "TextEncoder.encodeInto destination must be a Uint8Array",
            );
            return false;
        }
    };
    let result = unsafe { TextEncoder.encode_into(&input, dest.as_mut_slice()) };

    rooted!(&in(cx) let object = unsafe { JS_NewPlainObject(&mut cx) });
    if object.get().is_null() {
        report_error(
            &mut cx,
            "TextEncoder.encodeInto failed to allocate result object",
        );
        return false;
    }
    rooted!(&in(cx) let read = Int32Value(result.read_utf16 as i32));
    rooted!(&in(cx) let written = Int32Value(result.written as i32));
    let ok = unsafe {
        mozjs::rust::wrappers2::JS_DefineProperty(
            &mut cx,
            object.handle(),
            c"read".as_ptr(),
            read.handle(),
            0,
        ) && mozjs::rust::wrappers2::JS_DefineProperty(
            &mut cx,
            object.handle(),
            c"written".as_ptr(),
            written.handle(),
            0,
        )
    };
    if !ok {
        report_error(
            &mut cx,
            "TextEncoder.encodeInto failed to define result fields",
        );
        return false;
    }
    args.rval().set(ObjectValue(object.get()));
    true
}

unsafe extern "C" fn text_decoder_validate(
    raw_cx: *mut RawJSContext,
    argc: u32,
    vp: *mut Value,
) -> bool {
    let Some(mut cx) = callback_context(raw_cx) else {
        return false;
    };
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    let label = match arg_string(&mut cx, &args, 0, "TextDecoder label") {
        Some(label) => label,
        None => return false,
    };
    let fatal = arg_bool(&args, 1);
    let ignore_bom = arg_bool(&args, 2);
    if let Err(err) = TextDecoder::new(&label, fatal, ignore_bom) {
        report_error(&mut cx, &err.to_string());
        return false;
    }
    rooted!(&in(cx) let mut value = UndefinedValue());
    unsafe {
        "utf-8".to_jsval(cx.raw_cx(), value.handle_mut());
    }
    args.rval().set(value.get());
    true
}

unsafe extern "C" fn text_decoder_decode(
    raw_cx: *mut RawJSContext,
    argc: u32,
    vp: *mut Value,
) -> bool {
    let Some(mut cx) = callback_context(raw_cx) else {
        return false;
    };
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    let label = match arg_string(&mut cx, &args, 0, "TextDecoder label") {
        Some(label) => label,
        None => return false,
    };
    let fatal = arg_bool(&args, 1);
    let ignore_bom = arg_bool(&args, 2);
    let Some(bytes_value) = arg_handle(&args, 3) else {
        report_error(&mut cx, "TextDecoder.decode input bytes are required");
        return false;
    };
    if !bytes_value.get().is_object() {
        report_error(&mut cx, "TextDecoder.decode input must be a Uint8Array");
        return false;
    }
    let bytes = match Uint8Array::from(bytes_value.get().to_object()) {
        Ok(bytes) => bytes.to_vec(),
        Err(()) => {
            report_error(&mut cx, "TextDecoder.decode input must be a Uint8Array");
            return false;
        }
    };
    let decoder = match TextDecoder::new(&label, fatal, ignore_bom) {
        Ok(decoder) => decoder,
        Err(err) => {
            report_error(&mut cx, &err.to_string());
            return false;
        }
    };
    let output = match decoder.decode(&bytes) {
        Ok(output) => output,
        Err(err) => {
            report_error(&mut cx, &err.to_string());
            return false;
        }
    };
    rooted!(&in(cx) let mut value = UndefinedValue());
    unsafe {
        output.to_jsval(cx.raw_cx(), value.handle_mut());
    }
    args.rval().set(value.get());
    true
}

fn callback_context(raw_cx: *mut RawJSContext) -> Option<JSContext> {
    NonNull::new(raw_cx).map(|cx| unsafe { JSContext::from_ptr(cx) })
}

fn arg_handle(args: &CallArgs, index: u32) -> Option<mozjs::rust::HandleValue<'_>> {
    (index < args.argc_).then(|| unsafe { Handle::from_raw(args.get(index)) })
}

fn arg_string(cx: &mut JSContext, args: &CallArgs, index: u32, name: &str) -> Option<String> {
    let Some(value) = arg_handle(args, index) else {
        report_error(cx, &format!("{name} is required"));
        return None;
    };
    match String::safe_from_jsval(cx, value, ()) {
        Ok(ConversionResult::Success(value)) => Some(value),
        Ok(ConversionResult::Failure(_)) => {
            report_error(cx, &format!("{name} must be string-convertible"));
            None
        }
        Err(()) => None,
    }
}

fn arg_bool(args: &CallArgs, index: u32) -> bool {
    arg_handle(args, index)
        .map(|value| unsafe { mozjs::rust::ToBoolean(value) })
        .unwrap_or(false)
}

fn report_error(cx: &mut JSContext, message: &str) {
    let Ok(message) = CString::new(message) else {
        unsafe {
            JS_ReportErrorUTF8(cx.raw_cx(), c"script host error".as_ptr());
        }
        return;
    };
    unsafe {
        JS_ReportErrorUTF8(cx.raw_cx(), c"%s".as_ptr(), message.as_ptr());
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

        // Phase 6 pilot: Encoding API constructors live in the SpiderMonkey
        // global and call back into vixen-engine::text_codec, not the Page
        // string-smoke matcher.
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
        // SpiderMonkey global as `document` / read-only `Element` objects.
        let page = Page::from_html(
            "file:///dom-host.html",
            "<html><head><title>DOM host</title></head><body><p id='lead' class='note' data-role='copy'>Hello <b>world</b></p></body></html>",
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
            rt.evaluate_with_page(
                "document.getElementById('lead').getAttribute('data-role')",
                &page
            )
            .unwrap(),
            JsValue::String("copy".to_owned())
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
            rt.evaluate_with_page("document.querySelector('#missing') === null", &page)
                .unwrap(),
            JsValue::Bool(true)
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
