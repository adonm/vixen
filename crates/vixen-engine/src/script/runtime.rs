//! `deno_core` runtime construction and V8 value conversion.

#![forbid(unsafe_code)]

use deno_core::v8;
use deno_core::{JsRuntime as DenoJsRuntime, RuntimeOptions};

use crate::engine_error::{EngineError, codes};
use crate::page::Page;

use super::{JsValue, cssom, dom, encoding, webapi, webidl};

pub(super) fn new_deno_runtime(page: Option<&Page>) -> Result<DenoJsRuntime, EngineError> {
    let mut extensions = vec![
        webidl::extension(),
        encoding::extension(),
        webapi::extension(),
    ];
    if let Some(page) = page {
        extensions.push(dom::extension(page)?);
        extensions.push(cssom::extension(page)?);
    }

    DenoJsRuntime::try_new(RuntimeOptions {
        extensions,
        ..Default::default()
    })
    .map_err(|err| EngineError::Other {
        code: codes::SCRIPT_OOM,
        message: format!("deno_core runtime initialisation failed: {err}"),
    })
}

pub(super) fn js_value_from_global(
    runtime: &mut DenoJsRuntime,
    global: v8::Global<v8::Value>,
) -> Result<JsValue, EngineError> {
    deno_core::scope!(scope, runtime);
    let value = v8::Local::new(scope, global);

    if value.is_undefined() {
        Ok(JsValue::Undefined)
    } else if value.is_null() {
        Ok(JsValue::Null)
    } else if value.is_boolean() {
        Ok(JsValue::Bool(value.boolean_value(scope)))
    } else if value.is_int32() {
        Ok(JsValue::Int32(value.int32_value(scope).unwrap_or_default()))
    } else if value.is_number() {
        Ok(JsValue::Number(
            value.number_value(scope).unwrap_or(f64::NAN),
        ))
    } else if value.is_string() {
        let value = value.to_string(scope).ok_or_else(|| {
            EngineError::script(codes::SCRIPT_EVAL, "failed to convert JS string")
        })?;
        Ok(JsValue::String(value.to_rust_string_lossy(scope)))
    } else {
        Ok(JsValue::Object)
    }
}
