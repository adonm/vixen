//! `deno_core` runtime construction and V8 value conversion.

#![forbid(unsafe_code)]

use deno_core::v8;
use deno_core::{JsRuntime as DenoJsRuntime, PollEventLoopOptions, RuntimeOptions};
use vixen_net::NetworkConfig;

use crate::engine_error::{EngineError, codes};
use crate::page::Page;

use super::{JsValue, cssom, dom, encoding, webapi, webidl};

pub(super) struct DenoRuntimeInit {
    pub(super) runtime: DenoJsRuntime,
    pub(super) dom_mutations: Option<dom::DomMutationSink>,
}

pub(super) fn new_deno_runtime(
    page: Option<&Page>,
    network_config: NetworkConfig,
    storage: webapi::WebStorageHost,
) -> Result<DenoRuntimeInit, EngineError> {
    let fetch_policy = page.map(webapi::FetchPolicy::from_page);
    let mut extensions = vec![
        webidl::extension(),
        encoding::extension(),
        webapi::extension(network_config, storage, fetch_policy),
    ];
    let mut dom_mutations = None;
    if let Some(page) = page {
        let mutations = dom::DomMutationSink::default();
        extensions.push(dom::extension(page, mutations.clone())?);
        extensions.push(cssom::extension(page)?);
        dom_mutations = Some(mutations);
    }

    let runtime = DenoJsRuntime::try_new(RuntimeOptions {
        extensions,
        ..Default::default()
    })
    .map_err(|err| EngineError::Other {
        code: codes::SCRIPT_OOM,
        message: format!("deno_core runtime initialisation failed: {err}"),
    })?;
    Ok(DenoRuntimeInit {
        runtime,
        dom_mutations,
    })
}

pub(super) fn resolve_value(
    runtime: &mut DenoJsRuntime,
    global: v8::Global<v8::Value>,
) -> Result<v8::Global<v8::Value>, EngineError> {
    let resolve = runtime.resolve(global);
    let value = deno_core::futures::executor::block_on(
        runtime.with_event_loop_promise(resolve, PollEventLoopOptions::default()),
    )
    .map_err(|_| {
        EngineError::script(codes::SCRIPT_EVAL, "script evaluation raised an exception")
    })?;
    runtime.v8_isolate().perform_microtask_checkpoint();
    Ok(value)
}

pub(super) fn js_value_from_global(
    runtime: &mut DenoJsRuntime,
    global: v8::Global<v8::Value>,
) -> Result<JsValue, EngineError> {
    deno_core::scope!(scope, runtime);
    let value = v8::Local::new(scope, &global);

    let converted = if value.is_undefined() {
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
    };
    drop(global);
    converted
}
