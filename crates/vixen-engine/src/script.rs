//! `deno_core` runtime — the script execution boundary.
//!
//! The public Vixen-facing seam stays small (`JsRuntime`, `JsValue`, eval
//! methods), but the implementation uses `deno_core`/V8 directly per ADR-014.
//! Host surfaces are installed from focused bootstrap modules before the caller's
//! script runs. A `JsRuntime` owns a persistent V8 realm: sequential evals share
//! globals, storage host state, pending microtasks, and network host state until
//! the caller switches between the page and non-page realms or navigates to a new
//! page snapshot.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::doc::{DocumentScriptItem, ExternalScript};
use crate::engine_error::{EngineError, codes};
use crate::mime::MimeType;
use crate::page::Page;
use crate::storage_key::{StorageKind, StoragePartition};

mod cssom;
mod dom;
mod encoding;
mod runtime;
mod webapi;
mod webidl;

/// Vixen's JavaScript runtime seam, backed by `deno_core`/V8.
pub struct JsRuntime {
    network_config: vixen_net::NetworkConfig,
    storage_backend: webapi::WebStorageBackend,
    storage_temp_path: Option<PathBuf>,
    storage_session_id: String,
    storage_opaque_serial: u64,
    runtime: Option<deno_core::JsRuntime>,
    dom_mutations: Option<dom::DomMutationSink>,
    realm_key: RealmKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RealmKey {
    NoPage,
    Page(String),
}

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

/// A console event captured from the current JS realm.
#[derive(Debug, Clone, PartialEq)]
pub struct JsConsoleEvent {
    pub kind: String,
    pub args: Vec<JsConsoleArg>,
}

/// A CDP-friendly projection of a single console argument.
#[derive(Debug, Clone, PartialEq)]
pub struct JsConsoleArg {
    pub type_name: String,
    pub subtype: Option<String>,
    pub value: Option<JsConsoleValue>,
    pub unserializable_value: Option<String>,
    pub description: String,
}

/// A page modal dialog event captured from the current JS realm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsDialogEvent {
    pub kind: String,
    pub message: String,
    pub default_prompt: String,
}

/// A CDP runtime binding call captured from the current JS realm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsBindingEvent {
    pub name: String,
    pub payload: String,
}

/// JSON-scalar console argument values preserved across the runtime boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum JsConsoleValue {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}

/// A host-visible navigation/history action queued by the page realm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsNavigationAction {
    Navigate {
        url: String,
        replace: bool,
    },
    SetContent {
        html: String,
    },
    FormSubmit {
        form_id: String,
        form_node_id: usize,
        submitter_id: Option<String>,
        action: String,
        method: String,
    },
    HistoryPush {
        url: String,
        state_json: String,
        title: String,
    },
    HistoryReplace {
        url: String,
        state_json: String,
        title: String,
    },
    HistoryTraverse {
        delta: i32,
    },
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
        Self::with_network_config(vixen_net::NetworkConfig::default())
    }

    /// Initialise the runtime with a specific network configuration. This is
    /// primarily a deterministic-test seam; production uses [`Self::new`].
    pub fn with_network_config(
        network_config: vixen_net::NetworkConfig,
    ) -> Result<Self, EngineError> {
        let (storage_backend, storage_temp_path) = temporary_storage_backend()?;
        Self::with_storage_backend(network_config, storage_backend, storage_temp_path)
    }

    /// Initialise the runtime with persistent Web Storage at `path`.
    pub fn with_storage_path(path: impl AsRef<Path>) -> Result<Self, EngineError> {
        Self::with_network_config_and_storage_path(vixen_net::NetworkConfig::default(), path)
    }

    /// Initialise the runtime with both deterministic network config and a
    /// persistent `vixen-store` Web Storage database.
    pub fn with_network_config_and_storage_path(
        network_config: vixen_net::NetworkConfig,
        path: impl AsRef<Path>,
    ) -> Result<Self, EngineError> {
        let storage_backend =
            webapi::WebStorageBackend::open(path.as_ref()).map_err(|message| {
                EngineError::Other {
                    code: codes::SCRIPT_EVAL,
                    message: format!("Web Storage store initialisation failed: {message}"),
                }
            })?;
        Self::with_storage_backend(network_config, storage_backend, None)
    }

    fn with_storage_backend(
        network_config: vixen_net::NetworkConfig,
        storage_backend: webapi::WebStorageBackend,
        storage_temp_path: Option<PathBuf>,
    ) -> Result<Self, EngineError> {
        let storage_session_id = next_storage_session_id();
        let storage_opaque_serial = 1;
        let init = runtime::new_deno_runtime(
            None,
            network_config.clone(),
            web_storage_host(
                None,
                &storage_backend,
                &storage_session_id,
                storage_opaque_serial,
            ),
        )?;
        Ok(Self {
            network_config,
            storage_backend,
            storage_temp_path,
            storage_session_id,
            storage_opaque_serial,
            runtime: Some(init.runtime),
            dom_mutations: init.dom_mutations,
            realm_key: RealmKey::NoPage,
        })
    }

    /// Evaluate `src` in the persistent non-page JS global and return the
    /// result. Switching from a page realm resets to the non-page realm.
    pub fn evaluate(&mut self, src: &str) -> Result<JsValue, EngineError> {
        self.evaluate_with_page_context(src, None)
    }

    /// Evaluate `src` in a persistent page JS global with read-only DOM host
    /// objects projected from `page`. Reuses the realm for the same page
    /// snapshot; changing the page snapshot resets the page realm.
    pub fn evaluate_with_page(&mut self, src: &str, page: &Page) -> Result<JsValue, EngineError> {
        self.evaluate_with_page_context(src, Some(page))
    }

    /// Evaluate in a persistent page realm and commit supported DOM mutations
    /// back to the authoritative [`Page`] after the script completes.
    pub fn evaluate_with_page_mut(
        &mut self,
        src: &str,
        page: &mut Page,
    ) -> Result<JsValue, EngineError> {
        let value = self.evaluate_with_page_context(src, Some(&*page))?;
        self.apply_dom_mutations(page)?;
        Ok(value)
    }

    /// Execute classic page scripts in document order, using the persistent
    /// page realm for `page`.
    ///
    /// This is the page-script trust boundary: response-header CSP is active
    /// first, document meta CSP takes effect for later scripts as it is
    /// encountered, external scripts resolve against the document base URL,
    /// HTTP(S) fetches cross `vixen-net` URL policy, and `nosniff` is enforced
    /// before execution. Blocked/failed subresources are skipped; JavaScript
    /// exceptions still surface as [`codes::SCRIPT_EVAL`] errors.
    pub fn execute_page_scripts(&mut self, page: &mut Page) -> Result<usize, EngineError> {
        let items = page.document().script_execution_items();
        if !items.iter().any(|item| {
            matches!(
                item,
                DocumentScriptItem::InlineClassicScript(_)
                    | DocumentScriptItem::ExternalClassicScript(_)
            )
        }) {
            return Ok(0);
        }

        let mut csp = page.csp().clone();
        let origin = page_origin(page);
        let mut executed = 0;
        for item in items {
            match item {
                DocumentScriptItem::CspMeta(policy) => csp.add_header(&policy),
                DocumentScriptItem::InlineClassicScript(script) => {
                    match evaluate_inline_page_script(
                        self,
                        Some(&csp),
                        &origin,
                        page,
                        &script.source,
                        script.nonce.as_deref(),
                    ) {
                        Ok(_) => executed += 1,
                        Err(err) if err.code() == codes::SCRIPT_CSP_BLOCKED => {}
                        Err(err) => return Err(err),
                    }
                }
                DocumentScriptItem::ExternalClassicScript(script) => {
                    if let Some(source) = load_external_page_script(
                        &self.network_config,
                        &csp,
                        &origin,
                        page,
                        &script,
                    )? {
                        self.evaluate_with_page_mut(&source, page)?;
                        executed += 1;
                    }
                }
            }
        }
        Ok(executed)
    }

    /// Drop the current JavaScript realm while preserving runtime configuration
    /// such as deterministic network settings. The next evaluation creates a
    /// fresh global. Browser navigations use this so page scripts/listeners from
    /// the previous document cannot leak into the new document, even when the
    /// new URL and DOM snapshot are byte-for-byte identical.
    pub fn reset_realm(&mut self) {
        self.runtime = None;
        self.dom_mutations = None;
        self.realm_key = RealmKey::NoPage;
    }

    /// Drain console calls recorded in the current realm. CDP uses this after
    /// `Runtime.evaluate`, page-script execution, and synthetic input dispatch;
    /// callers that have not created a realm simply get an empty list.
    pub fn drain_console_events(&mut self) -> Result<Vec<JsConsoleEvent>, EngineError> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(Vec::new());
        };
        let result = runtime
            .execute_script(
                "vixen-console-drain.js",
                "JSON.stringify(globalThis.__vixenDrainConsoleEvents ? globalThis.__vixenDrainConsoleEvents() : [])".to_owned(),
            )
            .map_err(|_| {
                EngineError::script(codes::SCRIPT_EVAL, "failed to drain console events")
            })?;
        let result = runtime::resolve_value(runtime, result)?;
        match runtime::js_value_from_global(runtime, result)? {
            JsValue::String(json) => parse_console_events(&json),
            _ => Ok(Vec::new()),
        }
    }

    /// Drain modal dialogs recorded in the current realm. CDP turns these into
    /// `Page.javascriptDialogOpening` notifications.
    pub fn drain_dialog_events(&mut self) -> Result<Vec<JsDialogEvent>, EngineError> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(Vec::new());
        };
        let result = runtime
            .execute_script(
                "vixen-dialog-drain.js",
                "JSON.stringify(globalThis.__vixenDrainDialogEvents ? globalThis.__vixenDrainDialogEvents() : [])".to_owned(),
            )
            .map_err(|_| {
                EngineError::script(codes::SCRIPT_EVAL, "failed to drain dialog events")
            })?;
        let result = runtime::resolve_value(runtime, result)?;
        match runtime::js_value_from_global(runtime, result)? {
            JsValue::String(json) => parse_dialog_events(&json),
            _ => Ok(Vec::new()),
        }
    }

    /// Drain CDP runtime binding calls recorded in the current realm.
    pub fn drain_binding_events(&mut self) -> Result<Vec<JsBindingEvent>, EngineError> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(Vec::new());
        };
        let result = runtime
            .execute_script(
                "vixen-binding-drain.js",
                "JSON.stringify(globalThis.__vixenDrainBindingEvents ? globalThis.__vixenDrainBindingEvents() : [])".to_owned(),
            )
            .map_err(|_| {
                EngineError::script(codes::SCRIPT_EVAL, "failed to drain binding events")
            })?;
        let result = runtime::resolve_value(runtime, result)?;
        match runtime::js_value_from_global(runtime, result)? {
            JsValue::String(json) => parse_binding_events(&json),
            _ => Ok(Vec::new()),
        }
    }

    /// Drain navigation/history/form-submit actions recorded in the current
    /// page realm. Non-page realms and pages without queued actions return an
    /// empty list.
    pub fn drain_navigation_actions(&mut self) -> Result<Vec<JsNavigationAction>, EngineError> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(Vec::new());
        };
        let result = runtime
            .execute_script(
                "vixen-navigation-drain.js",
                "JSON.stringify(globalThis.__vixenDrainNavigationActions ? globalThis.__vixenDrainNavigationActions() : [])".to_owned(),
            )
            .map_err(|_| {
                EngineError::script(codes::SCRIPT_EVAL, "failed to drain navigation actions")
            })?;
        let result = runtime::resolve_value(runtime, result)?;
        match runtime::js_value_from_global(runtime, result)? {
            JsValue::String(json) => parse_navigation_actions(&json),
            _ => Ok(Vec::new()),
        }
    }

    /// Keep the persistent page realm associated with `page` after the host has
    /// applied a same-document navigation that JS already reflected locally.
    pub fn sync_page_realm_key(&mut self, page: &Page) {
        if self.runtime.is_some() {
            self.realm_key = RealmKey::Page(page_realm_key(page));
        }
    }

    fn evaluate_with_page_context(
        &mut self,
        src: &str,
        page: Option<&Page>,
    ) -> Result<JsValue, EngineError> {
        self.ensure_realm(page)?;

        let runtime = self.runtime.as_mut().expect("realm initialised");
        let result = runtime
            .execute_script("inline.js", src.to_owned())
            .map_err(|_| {
                EngineError::script(codes::SCRIPT_EVAL, "script evaluation raised an exception")
            })?;
        let result = runtime::resolve_value(runtime, result)?;
        runtime::js_value_from_global(runtime, result)
    }

    fn ensure_realm(&mut self, page: Option<&Page>) -> Result<(), EngineError> {
        let target = page
            .map(page_realm_key)
            .map(RealmKey::Page)
            .unwrap_or(RealmKey::NoPage);
        if self.realm_key != target || self.runtime.is_none() {
            self.runtime = None;
            self.storage_opaque_serial = self.storage_opaque_serial.saturating_add(1);
            let storage = web_storage_host(
                page,
                &self.storage_backend,
                &self.storage_session_id,
                self.storage_opaque_serial,
            );
            let init = runtime::new_deno_runtime(page, self.network_config.clone(), storage)?;
            self.runtime = Some(init.runtime);
            self.dom_mutations = init.dom_mutations;
            self.realm_key = target;
        }
        Ok(())
    }

    fn apply_dom_mutations(&mut self, page: &mut Page) -> Result<(), EngineError> {
        let Some(sink) = self.dom_mutations.as_ref() else {
            return Ok(());
        };
        let mutations = sink.take();
        if mutations.is_empty() {
            return Ok(());
        }

        for mutation in mutations {
            match mutation {
                dom::DomMutation::SetDocumentTitle { value } => page
                    .set_title(&value)
                    .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?,
                dom::DomMutation::SetTextContent { node_id, value } => page
                    .set_element_text_content(node_id, &value)
                    .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?,
                dom::DomMutation::SetAttribute {
                    node_id,
                    name,
                    value,
                } => page
                    .set_element_attribute(node_id, &name, &value)
                    .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?,
                dom::DomMutation::RemoveAttribute { node_id, name } => page
                    .remove_element_attribute(node_id, &name)
                    .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?,
                dom::DomMutation::SetInnerHtml { node_id, html } => page
                    .set_element_inner_html(node_id, &html)
                    .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?,
                dom::DomMutation::SetControlValue {
                    node_id,
                    element_id,
                    name,
                    tag,
                    value,
                } => page
                    .set_form_control_value(
                        node_id,
                        element_id.as_deref(),
                        name.as_deref(),
                        &tag,
                        &value,
                    )
                    .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?,
            }
        }
        self.realm_key = RealmKey::Page(page_realm_key(page));
        Ok(())
    }
}

fn parse_console_events(json: &str) -> Result<Vec<JsConsoleEvent>, EngineError> {
    let value: deno_core::serde_json::Value =
        deno_core::serde_json::from_str(json).map_err(|err| {
            EngineError::script(
                codes::SCRIPT_EVAL,
                format!("console event parse failed: {err}"),
            )
        })?;
    let Some(events) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(events.iter().map(parse_console_event).collect())
}

fn parse_console_event(value: &deno_core::serde_json::Value) -> JsConsoleEvent {
    JsConsoleEvent {
        kind: value
            .get("type")
            .and_then(deno_core::serde_json::Value::as_str)
            .unwrap_or("log")
            .to_owned(),
        args: value
            .get("args")
            .and_then(deno_core::serde_json::Value::as_array)
            .map(|args| args.iter().map(parse_console_arg).collect())
            .unwrap_or_default(),
    }
}

fn parse_console_arg(value: &deno_core::serde_json::Value) -> JsConsoleArg {
    JsConsoleArg {
        type_name: value
            .get("type")
            .and_then(deno_core::serde_json::Value::as_str)
            .unwrap_or("undefined")
            .to_owned(),
        subtype: value
            .get("subtype")
            .and_then(deno_core::serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        value: value.get("value").map(parse_console_value),
        unserializable_value: value
            .get("unserializableValue")
            .and_then(deno_core::serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        description: value
            .get("description")
            .and_then(deno_core::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

fn parse_console_value(value: &deno_core::serde_json::Value) -> JsConsoleValue {
    if let Some(s) = value.as_str() {
        JsConsoleValue::String(s.to_owned())
    } else if let Some(n) = value.as_f64() {
        JsConsoleValue::Number(n)
    } else if let Some(b) = value.as_bool() {
        JsConsoleValue::Bool(b)
    } else {
        JsConsoleValue::Null
    }
}

fn parse_dialog_events(json: &str) -> Result<Vec<JsDialogEvent>, EngineError> {
    let value: deno_core::serde_json::Value =
        deno_core::serde_json::from_str(json).map_err(|err| {
            EngineError::script(
                codes::SCRIPT_EVAL,
                format!("dialog event parse failed: {err}"),
            )
        })?;
    let Some(events) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(events.iter().map(parse_dialog_event).collect())
}

fn parse_dialog_event(value: &deno_core::serde_json::Value) -> JsDialogEvent {
    JsDialogEvent {
        kind: value
            .get("type")
            .and_then(deno_core::serde_json::Value::as_str)
            .unwrap_or("alert")
            .to_owned(),
        message: value
            .get("message")
            .and_then(deno_core::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        default_prompt: value
            .get("defaultPrompt")
            .and_then(deno_core::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

fn parse_binding_events(json: &str) -> Result<Vec<JsBindingEvent>, EngineError> {
    let value: deno_core::serde_json::Value =
        deno_core::serde_json::from_str(json).map_err(|err| {
            EngineError::script(
                codes::SCRIPT_EVAL,
                format!("binding event parse failed: {err}"),
            )
        })?;
    let Some(events) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(events.iter().map(parse_binding_event).collect())
}

fn parse_binding_event(value: &deno_core::serde_json::Value) -> JsBindingEvent {
    JsBindingEvent {
        name: value
            .get("name")
            .and_then(deno_core::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        payload: value
            .get("payload")
            .and_then(deno_core::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

fn parse_navigation_actions(json: &str) -> Result<Vec<JsNavigationAction>, EngineError> {
    let value: deno_core::serde_json::Value =
        deno_core::serde_json::from_str(json).map_err(|err| {
            EngineError::script(
                codes::SCRIPT_EVAL,
                format!("navigation action parse failed: {err}"),
            )
        })?;
    let Some(actions) = value.as_array() else {
        return Ok(Vec::new());
    };
    actions.iter().map(parse_navigation_action).collect()
}

fn parse_navigation_action(
    value: &deno_core::serde_json::Value,
) -> Result<JsNavigationAction, EngineError> {
    let kind = value
        .get("type")
        .and_then(deno_core::serde_json::Value::as_str)
        .unwrap_or_default();
    match kind {
        "navigate" => Ok(JsNavigationAction::Navigate {
            url: required_action_string(value, "url")?,
            replace: value
                .get("replace")
                .and_then(deno_core::serde_json::Value::as_bool)
                .unwrap_or(false),
        }),
        "set-content" => Ok(JsNavigationAction::SetContent {
            html: required_action_string(value, "html")?,
        }),
        "form-submit" => Ok(JsNavigationAction::FormSubmit {
            form_id: required_action_string(value, "formId")?,
            form_node_id: value
                .get("formNodeId")
                .and_then(deno_core::serde_json::Value::as_u64)
                .unwrap_or_default() as usize,
            submitter_id: value
                .get("submitterId")
                .and_then(deno_core::serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned),
            action: required_action_string(value, "action")?,
            method: value
                .get("method")
                .and_then(deno_core::serde_json::Value::as_str)
                .unwrap_or("get")
                .to_ascii_lowercase(),
        }),
        "history-push" => Ok(JsNavigationAction::HistoryPush {
            url: required_action_string(value, "url")?,
            state_json: value
                .get("stateJson")
                .and_then(deno_core::serde_json::Value::as_str)
                .unwrap_or("null")
                .to_owned(),
            title: value
                .get("title")
                .and_then(deno_core::serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }),
        "history-replace" => Ok(JsNavigationAction::HistoryReplace {
            url: required_action_string(value, "url")?,
            state_json: value
                .get("stateJson")
                .and_then(deno_core::serde_json::Value::as_str)
                .unwrap_or("null")
                .to_owned(),
            title: value
                .get("title")
                .and_then(deno_core::serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }),
        "history-traverse" => Ok(JsNavigationAction::HistoryTraverse {
            delta: value
                .get("delta")
                .and_then(deno_core::serde_json::Value::as_i64)
                .unwrap_or_default()
                .clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        }),
        other => Err(EngineError::script(
            codes::SCRIPT_EVAL,
            format!("unsupported navigation action: {other}"),
        )),
    }
}

fn required_action_string(
    value: &deno_core::serde_json::Value,
    name: &str,
) -> Result<String, EngineError> {
    value
        .get(name)
        .and_then(deno_core::serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            EngineError::script(
                codes::SCRIPT_EVAL,
                format!("navigation action missing string field `{name}`"),
            )
        })
}

fn page_realm_key(page: &Page) -> String {
    format!("{}\n{}", page.url(), page.dump_dom())
}

fn page_origin(page: &Page) -> vixen_net::Origin {
    url::Url::parse(page.url())
        .map(|url| vixen_net::Origin::from_url(&url))
        .unwrap_or_else(|_| vixen_net::Origin::opaque())
}

fn web_storage_host(
    page: Option<&Page>,
    backend: &webapi::WebStorageBackend,
    session_id: &str,
    opaque_serial: u64,
) -> webapi::WebStorageHost {
    webapi::WebStorageHost::new(
        backend.clone(),
        webapi::WebStoragePartitions {
            local: web_storage_partition_key(page, StorageKind::Local, session_id, opaque_serial),
            session: web_storage_partition_key(
                page,
                StorageKind::Session,
                session_id,
                opaque_serial,
            ),
        },
    )
}

fn web_storage_partition_key(
    page: Option<&Page>,
    kind: StorageKind,
    session_id: &str,
    opaque_serial: u64,
) -> String {
    let origin = page
        .map(page_origin)
        .unwrap_or_else(vixen_net::Origin::opaque);
    if !origin.is_opaque() {
        return StoragePartition::new(origin, kind).partition_key();
    }

    let document_key = page
        .map(page_realm_key)
        .unwrap_or_else(|| "no-page".to_owned());
    format!(
        "storage:{}:opaque:{}",
        kind.tag(),
        stable_storage_hash(&format!("{session_id}\n{opaque_serial}\n{document_key}"))
    )
}

fn stable_storage_hash(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

static STORAGE_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_storage_session_id() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn temporary_storage_backend() -> Result<(webapi::WebStorageBackend, Option<PathBuf>), EngineError>
{
    let path = std::env::temp_dir().join(format!(
        "vixen-js-storage-{}-{}.redb",
        std::process::id(),
        STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let backend = webapi::WebStorageBackend::open(&path).map_err(|message| EngineError::Other {
        code: codes::SCRIPT_EVAL,
        message: format!("Web Storage store initialisation failed: {message}"),
    })?;
    Ok((backend, Some(path)))
}

fn load_external_page_script(
    network_config: &vixen_net::NetworkConfig,
    csp: &vixen_net::csp::ContentSecurityPolicy,
    origin: &vixen_net::Origin,
    page: &Page,
    script: &ExternalScript,
) -> Result<Option<String>, EngineError> {
    let Some(script_url) = resolve_external_script_url(page, &script.src) else {
        return Ok(None);
    };
    if !csp.allows_external_script(origin, &script_url, script.nonce.as_deref()) {
        return Ok(None);
    }

    match script_url.scheme() {
        "file" => Ok(load_file_script(&script_url)),
        "http" | "https" => {
            let response = match fetch_http_script(network_config.clone(), script_url) {
                Ok(response) => response,
                Err(_) => return Ok(None),
            };
            if script_response_allowed(&response) {
                Ok(Some(response.body))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

fn resolve_external_script_url(page: &Page, src: &str) -> Option<url::Url> {
    url::Url::parse(&page.document_base_uri())
        .or_else(|_| url::Url::parse(page.url()))
        .ok()?
        .join(src)
        .ok()
}

fn load_file_script(url: &url::Url) -> Option<String> {
    let path = url.to_file_path().ok()?;
    std::fs::read_to_string(path).ok()
}

fn fetch_http_script(
    network_config: vixen_net::NetworkConfig,
    url: url::Url,
) -> Result<vixen_net::TextResponse, vixen_net::NetworkError> {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| vixen_net::NetworkError::Builder {
                message: err.to_string(),
            })?;
        rt.block_on(async move {
            let mut network = vixen_net::Network::new(network_config)?;
            let mut jar = vixen_net::CookieJar::default();
            network
                .get_text_with_cookies(&mut jar, &url, false, vixen_net::Method::Get)
                .await
        })
    })
    .join()
    .map_err(|_| vixen_net::NetworkError::Transport {
        message: "external script fetch thread panicked".to_owned(),
    })?
}

fn script_response_allowed(response: &vixen_net::TextResponse) -> bool {
    let nosniff = response
        .header("x-content-type-options")
        .is_some_and(vixen_net::is_nosniff);
    let mime_essence = response
        .header("content-type")
        .and_then(MimeType::parse)
        .map(|mime| mime.essence())
        .unwrap_or_else(|| "text/plain".to_owned());
    matches!(
        vixen_net::enforce_nosniff(nosniff, &mime_essence, vixen_net::Destination::Script),
        vixen_net::NosniffOutcome::Allow
    )
}

impl Default for JsRuntime {
    fn default() -> Self {
        Self::new().expect("deno_core runtime must initialise")
    }
}

impl Drop for JsRuntime {
    fn drop(&mut self) {
        self.runtime = None;
        self.storage_backend = webapi::WebStorageBackend::memory();
        if let Some(path) = self.storage_temp_path.take() {
            let _ = std::fs::remove_file(path);
        }
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
    enforce_inline_script_csp(csp, origin, src, nonce)?;
    rt.evaluate(src)
}

/// Evaluate an inline script in the persistent page realm after CSP approval.
pub fn evaluate_inline_page_script(
    rt: &mut JsRuntime,
    csp: Option<&vixen_net::csp::ContentSecurityPolicy>,
    origin: &vixen_net::Origin,
    page: &mut Page,
    src: &str,
    nonce: Option<&str>,
) -> Result<JsValue, EngineError> {
    enforce_inline_script_csp(csp, origin, src, nonce)?;
    rt.evaluate_with_page_mut(src, page)
}

fn enforce_inline_script_csp(
    csp: Option<&vixen_net::csp::ContentSecurityPolicy>,
    origin: &vixen_net::Origin,
    src: &str,
    nonce: Option<&str>,
) -> Result<(), EngineError> {
    if let Some(policy) = csp
        && !policy.allows_inline_script(origin, Some(src), nonce)
    {
        return Err(EngineError::script(
            codes::SCRIPT_CSP_BLOCKED,
            "inline script blocked by Content-Security-Policy",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_fetch_server(
        host: &str,
        body: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_owned();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Vixen-Test: yes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/payload", addr.port()),
            config,
            handle,
        )
    }

    fn spawn_script_server(
        host: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_owned();
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let mut response = "HTTP/1.1 200 OK\r\n".to_owned();
            for (name, value) in headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ));
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (format!("http://{host}:{}", addr.port()), config, handle)
    }

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
            rt.evaluate("new TextEncoder() instanceof TextEncoder")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("Object.prototype.toString.call(new TextDecoder())")
                .unwrap(),
            JsValue::String("[object TextDecoder]".to_owned())
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

        // Generated WebIDL scaffolding exposes browser-shaped constructors and
        // prototype inheritance before backend behavior is implemented.
        let all_webidl_constructors = webidl_all_constructors_expr();
        let all_webidl_parent_chains = webidl_parent_chains_expr();
        assert_eq!(
            rt.evaluate(&all_webidl_constructors).unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate(&all_webidl_parent_chains).unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("['Window','Document','HTMLElement','HTMLDialogElement','CanvasRenderingContext2D','CSSStyleDeclaration','Request','ReadableStream','GPUDevice','PaymentRequest','IDBDatabase'].every((name) => typeof globalThis[name] === 'function')")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("Window.prototype instanceof EventTarget && HTMLDialogElement.prototype instanceof HTMLElement && CSSStyleRule.prototype instanceof CSSRule && GPUDevice.prototype instanceof EventTarget && IDBDatabase.prototype instanceof EventTarget")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("HTMLElement.prototype instanceof Element && XMLDocument.prototype instanceof Document")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("typeof HTMLDialogElement.prototype.showModal === 'function' && 'innerText' in HTMLElement.prototype && 'getContext' in HTMLCanvasElement.prototype")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("(() => { try { new HTMLDialogElement(); } catch (err) { return err instanceof TypeError && /Illegal constructor: HTMLDialogElement/.test(err.message); } return false; })()")
                .unwrap(),
            JsValue::Bool(true)
        );

        // Pure Web API value objects are runtime constructors, not Page-string
        // parser special cases.
        assert_eq!(
            rt.evaluate("new Event('message').type").unwrap(),
            JsValue::String("message".to_owned())
        );
        assert_eq!(
            rt.evaluate("new Event('message', { bubbles: true, cancelable: true, composed: true }).cancelable")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("new Event('message').target === null").unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("new Event('message').composedPath().length")
                .unwrap(),
            JsValue::Int32(0)
        );
        assert_eq!(
            rt.evaluate("new CustomEvent('note', { detail: 'payload' }).detail")
                .unwrap(),
            JsValue::String("payload".to_owned())
        );
        assert_eq!(
            rt.evaluate("(() => { const t = new EventTarget(); let seen = 0; t.addEventListener('x', (e) => { if (e.type === 'x') seen++; }); return t.dispatchEvent(new Event('x')) && seen === 1; })()")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("new DOMPoint(1, 2, 3, 4).z").unwrap(),
            JsValue::Int32(3)
        );
        assert_eq!(
            rt.evaluate("DOMPoint.fromPoint({ x: 5, y: 6 }).w").unwrap(),
            JsValue::Int32(1)
        );
        assert_eq!(
            rt.evaluate("new DOMRect(10, 20, -5, 7).left").unwrap(),
            JsValue::Int32(5)
        );
        assert_eq!(
            rt.evaluate("DOMRect.fromRect({ x: 1, y: 2, width: 3, height: 4 }).bottom")
                .unwrap(),
            JsValue::Int32(6)
        );
        assert_eq!(
            rt.evaluate("DOMQuad.fromRect({ x: 1, y: 2, width: 3, height: 4 }).p3.x")
                .unwrap(),
            JsValue::Int32(4)
        );
        assert_eq!(
            rt.evaluate("DOMQuad.fromRect({ x: 1, y: 2, width: 3, height: 4 }).getBounds().height")
                .unwrap(),
            JsValue::Int32(4)
        );
        assert_eq!(
            rt.evaluate("new DOMMatrix([1, 0, 0, 1, 5, 6]).e").unwrap(),
            JsValue::Int32(5)
        );
        assert_eq!(
            rt.evaluate("new DOMMatrix().translate(10, 20).transformPoint(new DOMPoint(1, 2)).y")
                .unwrap(),
            JsValue::Int32(22)
        );
        assert_eq!(
            rt.evaluate("new DOMMatrix().scale(2, 3).transformPoint(new DOMPoint(5, 5)).x")
                .unwrap(),
            JsValue::Int32(10)
        );
        assert_eq!(
            rt.evaluate(
                "new DOMMatrix().translate(10, 20).inverse().transformPoint(new DOMPoint(10, 20)).x"
            )
            .unwrap(),
            JsValue::Int32(0)
        );
        assert_eq!(
            rt.evaluate("Number.isNaN(new DOMMatrix([0, 0, 0, 0, 0, 0]).inverse().m11)")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("new DOMMatrix().setMatrixValue('matrix(1, 0, 0, 1, 5, 6)').e")
                .unwrap(),
            JsValue::Int32(5)
        );
        assert_eq!(
            rt.evaluate("new DOMMatrix().setMatrixValue('none').isIdentity")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("new Headers([['Content-Type', ' text/plain '], ['X-Test', 'a'], ['X-Test', 'b']]).get('x-test')")
                .unwrap(),
            JsValue::String("a, b".to_owned())
        );
        assert_eq!(
            rt.evaluate("new Blob(['Hi', 'é'], { type: 'TEXT/PLAIN' }).size")
                .unwrap(),
            JsValue::Int32(4)
        );
        assert_eq!(
            rt.evaluate("new Blob([new Uint8Array([72, 105])]).text()")
                .unwrap(),
            JsValue::String("Hi".to_owned())
        );
        assert_eq!(
            rt.evaluate("new File(['hello'], 'note.txt', { type: 'text/plain', lastModified: 42 }).lastModified")
                .unwrap(),
            JsValue::Int32(42)
        );
        assert_eq!(
            rt.evaluate("(() => { const dt = new DataTransfer(); dt.items.add(new File([new Uint8Array([65, 66])], 'ab.txt', { type: 'text/plain' })); return dt.files.length + ':' + dt.files[0].name + ':' + dt.files[0].size + ':' + dt.types[0]; })()")
                .unwrap(),
            JsValue::String("1:ab.txt:2:Files".to_owned())
        );
        assert_eq!(
            rt.evaluate("new Response('Created', { status: 201 }).headers.get('content-type')")
                .unwrap(),
            JsValue::String("text/plain;charset=UTF-8".to_owned())
        );
        assert_eq!(
            rt.evaluate("Response.json({ok:true}, { status: 201 }).headers.get('content-type')")
                .unwrap(),
            JsValue::String("application/json".to_owned())
        );
        assert_eq!(
            rt.evaluate("Response.error().status").unwrap(),
            JsValue::Int32(0)
        );
        assert_eq!(
            rt.evaluate(
                "Response.redirect('https://example.com/target', 302).headers.get('location')"
            )
            .unwrap(),
            JsValue::String("https://example.com/target".to_owned())
        );
        assert_eq!(
            rt.evaluate("new Request('https://example.com/api', { method: 'post', headers: [['Host', 'evil.test'], ['Accept', 'text/html']], body: 'hello' }).headers.has('host')")
                .unwrap(),
            JsValue::Bool(false)
        );
        assert_eq!(
            rt.evaluate("new URL('/other', 'https://example.com/app/page').href")
                .unwrap(),
            JsValue::String("https://example.com/other".to_owned())
        );
        assert_eq!(
            rt.evaluate("URL.canParse('/other', 'https://example.com/app/page')")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("URL.canParse('://bad')").unwrap(),
            JsValue::Bool(false)
        );
        assert_eq!(
            rt.evaluate("new URL('data:text/plain,Hello').origin")
                .unwrap(),
            JsValue::String("null".to_owned())
        );
        assert_eq!(
            rt.evaluate("new URLSearchParams('?q=rust+lang&tag=web&tag=engine').getAll('tag')[1]")
                .unwrap(),
            JsValue::String("engine".to_owned())
        );
        assert_eq!(
            rt.evaluate("new URLPattern({ pathname: '/posts/:id' }).exec({ pathname: '/posts/42' }).pathname.groups.id")
                .unwrap(),
            JsValue::String("42".to_owned())
        );
        assert_eq!(
            rt.evaluate("typeof performance.now()").unwrap(),
            JsValue::String("number".to_owned())
        );
        assert_eq!(
            rt.evaluate("performance.timeOrigin + performance.now() >= performance.timeOrigin")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("matchMedia('(min-width: 800px)').matches")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("navigator.userAgent.includes('Vixen')")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("navigator.languages[0]").unwrap(),
            JsValue::String("en-US".to_owned())
        );
        assert_eq!(
            rt.evaluate("(() => { localStorage.setItem('theme', 'dark'); return localStorage.getItem('theme') + ':' + localStorage.length + ':' + localStorage.key(0); })()")
                .unwrap(),
            JsValue::String("dark:1:theme".to_owned())
        );
        assert_eq!(
            rt.evaluate("(() => { localStorage.setItem('shared', 'local'); sessionStorage.setItem('shared', 'session'); return localStorage.getItem('shared') + ':' + sessionStorage.getItem('shared'); })()")
                .unwrap(),
            JsValue::String("local:session".to_owned())
        );
        assert_eq!(
            rt.evaluate("(() => { try { localStorage.setItem('', 'x'); } catch (err) { return err instanceof TypeError && /storage key must be non-empty/.test(err.message); } return false; })()")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("structuredClone(new Map([['answer', 42]])).get('answer')")
                .unwrap(),
            JsValue::Int32(42)
        );
        assert_eq!(
            rt.evaluate("structuredClone('hello')").unwrap(),
            JsValue::String("hello".to_owned())
        );
        assert_eq!(
            rt.evaluate("structuredClone([1,2,3]).length").unwrap(),
            JsValue::Int32(3)
        );
        assert_eq!(
            rt.evaluate("structuredClone({greeting:'hello'}).greeting")
                .unwrap(),
            JsValue::String("hello".to_owned())
        );
        assert_eq!(
            rt.evaluate("structuredClone(new Date(42)).getTime()")
                .unwrap(),
            JsValue::Int32(42)
        );
        assert_eq!(
            rt.evaluate("structuredClone(new Map([['answer', 42]])).entries().next().value[0]")
                .unwrap(),
            JsValue::String("answer".to_owned())
        );
        assert_eq!(
            rt.evaluate("structuredClone(new Set(['alpha','beta'])).has('beta')")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("structuredClone(new TypeError('boom')).message")
                .unwrap(),
            JsValue::String("boom".to_owned())
        );
        assert_eq!(
            rt.evaluate("structuredClone(new TypeError('boom')).name")
                .unwrap(),
            JsValue::String("TypeError".to_owned())
        );
        assert_eq!(
            rt.evaluate("new MutationObserver(() => {}).takeRecords().length")
                .unwrap(),
            JsValue::Int32(0)
        );
        assert_eq!(
            rt.evaluate("new MutationObserver(() => {}).disconnect()")
                .unwrap(),
            JsValue::Undefined
        );
        assert_eq!(
            rt.evaluate("new DOMParser().parseFromString(\"<main><p id='parsed'>Parsed</p></main>\", 'text/html').querySelector('#parsed').textContent")
                .unwrap(),
            JsValue::String("Parsed".to_owned())
        );
        assert_eq!(
            rt.evaluate("btoa('Vixen') + ':' + atob('Vml4ZW4=')")
                .unwrap(),
            JsValue::String("Vml4ZW4=:Vixen".to_owned())
        );
        assert_eq!(
            rt.evaluate("AbortSignal.any([AbortSignal.timeout(0)]).aborted")
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate("new AbortController().signal.aborted").unwrap(),
            JsValue::Bool(false)
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
            rt.evaluate_with_page("document instanceof Document", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document instanceof Node", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.defaultView === window", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.location.href", &page)
                .unwrap(),
            JsValue::String("file:///dom-host.html".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.baseURI", &page).unwrap(),
            JsValue::String("file:///dom-host.html".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.compatMode + ':' + document.characterSet + ':' + document.contentType + ':' + document.visibilityState + ':' + document.hidden + ':' + document.referrer + ':' + document.hasFocus()",
                &page
            )
            .unwrap(),
            JsValue::String("CSS1Compat:UTF-8:text/html:visible:false::true".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "location.href === document.location.href && window.location.href === document.URL",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.head instanceof HTMLHeadElement", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.body instanceof HTMLBodyElement", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.activeElement === document.body", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').textContent", &page)
                .unwrap(),
            JsValue::String("Hello world".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').innerHTML", &page)
                .unwrap(),
            JsValue::String("Hello <b>world</b>".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead') instanceof Element", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead') instanceof HTMLElement",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead') instanceof HTMLParagraphElement",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("HTMLElement.prototype instanceof Element", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(&all_webidl_constructors, &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(&all_webidl_parent_chains, &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').tagName", &page)
                .unwrap(),
            JsValue::String("P".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').namespaceURI + ':' + document.querySelector('#lead').prefix",
                &page
            )
            .unwrap(),
            JsValue::String("http://www.w3.org/1999/xhtml:null".to_owned())
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
            rt.evaluate_with_page("document.querySelectorAll('p') instanceof NodeList", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelectorAll('p').item(0) === document.querySelector('#lead')",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelectorAll('.note').length", &page)
                .unwrap(),
            JsValue::Int32(1)
        );
        assert_eq!(
            rt.evaluate_with_page("document.getElementsByTagName('p').length", &page)
                .unwrap(),
            JsValue::Int32(1)
        );
        assert_eq!(
            rt.evaluate_with_page("document.getElementsByClassName('note').length", &page)
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
            rt.evaluate_with_page("document.body.children instanceof HTMLCollection", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.body.children.length", &page)
                .unwrap(),
            JsValue::Int32(2)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.body.firstElementChild === document.querySelector('#lead')",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').parentElement === document.body",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').nextElementSibling === document.querySelector('#frame')",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').attributes instanceof NamedNodeMap",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').attributes.getNamedItem('data-role').value",
                &page
            )
            .unwrap(),
            JsValue::String("copy".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.forms.length + document.images.length + document.links.length + document.scripts.length", &page)
                .unwrap(),
            JsValue::Int32(0)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('p').matches('.note')", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('p.note') === document.querySelector('#lead')",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('body > p:is(.note, .missing):not(.missing)').id",
                &page
            )
            .unwrap(),
            JsValue::String("lead".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('p:has(> b)').id", &page)
                .unwrap(),
            JsValue::String("lead".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelectorAll('body > :where(p, iframe)').length",
                &page
            )
            .unwrap(),
            JsValue::Int32(2)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').matches('body > p:has(> b)')",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#lead').classList.length", &page)
                .unwrap(),
            JsValue::Int32(2)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').classList instanceof DOMTokenList",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
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
                "document.querySelector('#lead').dataset instanceof DOMStringMap",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
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

        let image_page = Page::from_html(
            "file:///dom-image-host.html",
            "<img id='widths' src='small.jpg' srcset='small.jpg 480w, medium.jpg 800w, large.jpg 1200w' sizes='100vw'>\
             <img id='density' srcset='one.png 1x, two.png 2x'>\
             <img id='fallback' src='fallback.jpg'>",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#widths').currentSrc", &image_page)
                .unwrap(),
            JsValue::String("medium.jpg".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.querySelector('#density').currentSrc", &image_page)
                .unwrap(),
            JsValue::String("one.png".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#fallback').currentSrc",
                &image_page
            )
            .unwrap(),
            JsValue::String("fallback.jpg".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.createRange().collapsed", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.createRange().startOffset", &page)
                .unwrap(),
            JsValue::Int32(0)
        );
        assert_eq!(
            rt.evaluate_with_page("window.getSelection().rangeCount", &page)
                .unwrap(),
            JsValue::Int32(0)
        );
        assert_eq!(
            rt.evaluate_with_page("document.getSelection().isCollapsed", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#lead').dispatchEvent(new CustomEvent('click', { detail: 'payload' }))",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
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
                "getComputedStyle(document.querySelector('#lead')) instanceof CSSStyleDeclaration",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
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
            rt.evaluate_with_page("document.styleSheets instanceof StyleSheetList", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].cssRules.length", &page)
                .unwrap(),
            JsValue::Int32(2)
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0] instanceof CSSStyleSheet", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.styleSheets[0].cssRules instanceof CSSRuleList",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].href === null", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].disabled", &page)
                .unwrap(),
            JsValue::Bool(false)
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].ownerNode.tagName", &page)
                .unwrap(),
            JsValue::String("STYLE".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].cssRules[0].selectorText", &page)
                .unwrap(),
            JsValue::String("#lead".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.styleSheets[0].cssRules[0] instanceof CSSStyleRule",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.styleSheets[0].cssRules[0] instanceof CSSRule",
                &page
            )
            .unwrap(),
            JsValue::Bool(true)
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
        assert_eq!(
            rt.evaluate_with_page("document.styleSheets[0].cssRules[0].style[1]", &page)
                .unwrap(),
            JsValue::String("font-size".to_owned())
        );

        let geometry_page = Page::from_html(
            "file:///dom-geometry-host.html",
            "<style>#box { width: 40px; height: 20px; }</style><main><div id='box'>Box</div><input id='empty'><input id='check' type='checkbox'><button id='named'>Hit me</button></main>",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#box').getBoundingClientRect().x",
                &geometry_page
            )
            .unwrap(),
            JsValue::Int32(8)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#box').getBoundingClientRect().width",
                &geometry_page
            )
            .unwrap(),
            JsValue::Int32(40)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#box').getBoundingClientRect() instanceof DOMRectReadOnly",
                &geometry_page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#box').getBoundingClientRect().right",
                &geometry_page
            )
            .unwrap(),
            JsValue::Int32(48)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#box').getBoundingClientRect().toJSON().height",
                &geometry_page
            )
            .unwrap(),
            JsValue::Int32(20)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#box').getClientRects().length",
                &geometry_page
            )
            .unwrap(),
            JsValue::Int32(1)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#box').getClientRects() instanceof DOMRectList",
                &geometry_page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#box').getClientRects().item(0).width",
                &geometry_page
            )
            .unwrap(),
            JsValue::Int32(40)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.documentElement.parentNode === document && document.documentElement.parentElement === null",
                &geometry_page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#box').getRootNode() === document && document.contains(document.querySelector('#box'))",
                &geometry_page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "Node.ELEMENT_NODE + ':' + Node.TEXT_NODE + ':' + document.nodeType",
                &geometry_page
            )
            .unwrap(),
            JsValue::String("1:3:9".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.elementFromPoint(10, 10).id", &geometry_page)
                .unwrap(),
            JsValue::String("box".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("document.elementsFromPoint(10, 10)[0].id", &geometry_page)
                .unwrap(),
            JsValue::String("box".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#box').scrollIntoView(); 'done'",
                &geometry_page
            )
            .unwrap(),
            JsValue::String("done".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#empty').getBoundingClientRect().width > 0",
                &geometry_page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#check').getBoundingClientRect().width",
                &geometry_page
            )
            .unwrap(),
            JsValue::Int32(13)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const text = document.querySelector('#named').firstChild; return document.querySelector('#named').childNodes.length + ':' + text.nodeType + ':' + text.data + ':' + (text.parentNode.id); })()",
                &geometry_page
            )
            .unwrap(),
            JsValue::String("1:3:Hit me:named".to_owned())
        );

        let actionability_page = Page::from_html(
            "file:///dom-actionability-host.html",
            "<form><label>Typed name <input id='typed-name'></label><label>Typed body <textarea id='typed-body'></textarea></label></form>",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const input = document.querySelector('#typed-name'); const r = input.getBoundingClientRect(); const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2); return hit && hit.id; })()",
                &actionability_page
            )
            .unwrap(),
            JsValue::String("typed-name".to_owned())
        );

        let form_page = Page::from_html(
            "file:///dom-formdata-host.html",
            "<form id='contact'><label id='name-label' for='name-input'>Name</label><input id='name-input' name='name' value='Ada'><label id='body-label'>Body<textarea name='body'>Hello</textarea></label><input type='checkbox' name='format' value='html' checked><select name='plan'><option value='free'>Free</option><option value='pro' selected>Pro</option></select></form><form id='upload' enctype='multipart/form-data'><input type='file' name='attachment'></form>",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.querySelector('#contact')).get('name')",
                &form_page
            )
            .unwrap(),
            JsValue::String("Ada".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.querySelector('#contact')).get('body')",
                &form_page
            )
            .unwrap(),
            JsValue::String("Hello".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.querySelector('#contact')).get('plan')",
                &form_page
            )
            .unwrap(),
            JsValue::String("pro".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.querySelector('#contact')).getAll('format').length",
                &form_page
            )
            .unwrap(),
            JsValue::Int32(1)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.querySelector('#contact')).has('skip')",
                &form_page
            )
            .unwrap(),
            JsValue::Bool(false)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.querySelector('#contact')).get('missing') === null",
                &form_page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.querySelector('#contact')).entries().next().value[0]",
                &form_page
            )
            .unwrap(),
            JsValue::String("name".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.querySelector('#contact')).entries().next().value[1]",
                &form_page
            )
            .unwrap(),
            JsValue::String("Ada".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.querySelector('#contact')).keys().next().value",
                &form_page
            )
            .unwrap(),
            JsValue::String("name".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.querySelector('#contact')).values().next().value",
                &form_page
            )
            .unwrap(),
            JsValue::String("Ada".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.getElementById('upload')).get('attachment').type",
                &form_page
            )
            .unwrap(),
            JsValue::String("application/octet-stream".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "new FormData(document.getElementById('upload')).get('attachment').size",
                &form_page
            )
            .unwrap(),
            JsValue::Int32(0)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const form = document.getElementById('upload'); const input = form.querySelector('input'); const dt = new DataTransfer(); dt.items.add(new File([new Uint8Array([65, 66])], 'ab.txt', { type: 'text/plain' })); input.files = dt.files; const file = new FormData(form).get('attachment'); return input.files.length + ':' + input.files[0].name + ':' + input.value + ':' + file.name + ':' + file.size + ':' + file.type; })()",
                &form_page
            )
            .unwrap(),
            JsValue::String("1:ab.txt:C:\\fakepath\\ab.txt:ab.txt:2:text/plain".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const input = document.querySelector('[name=name]'); const textarea = document.querySelector('[name=body]'); return input.type + ':' + input.disabled + ':' + input.readOnly + ':' + input.isContentEditable + ':' + input.name + ':' + textarea.type; })()",
                &form_page
            )
            .unwrap(),
            JsValue::String("text:false:false:false:name:textarea".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const input = document.querySelector('[name=name]'); input.disabled = true; input.readOnly = true; return input.hasAttribute('disabled') + ':' + input.disabled + ':' + input.hasAttribute('readonly') + ':' + input.readOnly; })()",
                &form_page
            )
            .unwrap(),
            JsValue::String("true:true:true:true".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const input = document.querySelector('[name=name]'); const textarea = document.querySelector('[name=body]'); const explicit = document.querySelector('#name-label'); const nested = document.querySelector('#body-label'); return explicit.htmlFor + ':' + explicit.control.id + ':' + input.labels.length + ':' + input.labels[0].id + ':' + nested.control.name + ':' + textarea.labels[0].id; })()",
                &form_page
            )
            .unwrap(),
            JsValue::String("name-input:name-input:1:name-label:body:body-label".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const explicit = document.querySelector('#name-label'); const nested = document.querySelector('#body-label'); return explicit.firstChild.nodeType + ':' + explicit.firstChild.nodeValue + ':' + nested.firstChild.nodeValue; })()",
                &form_page
            )
            .unwrap(),
            JsValue::String("3:Name:Body".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const select = document.querySelector('[name=plan]'); return select.options.length + ':' + select.length + ':' + select.size + ':' + select.value + ':' + select.selectedIndex + ':' + select.selectedOptions[0].label + ':' + select.options[1].index; })()",
                &form_page
            )
            .unwrap(),
            JsValue::String("2:2:0:pro:1:Pro:1".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const select = document.querySelector('[name=plan]'); select.options[0].selected = true; return select.value + ':' + select.selectedIndex + ':' + select.options[1].selected; })()",
                &form_page
            )
            .unwrap(),
            JsValue::String("free:0:false".to_owned())
        );

        let reflected_form_page = Page::from_html(
            "file:///dom-reflected-form-host.html",
            "<form id='f' method='POST' enctype='multipart/form-data' action='/submit'></form><form id='default'></form>",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const form = document.querySelector('#f'); return form.method + ':' + form.enctype + ':' + form.encoding + ':' + form.action; })()",
                &reflected_form_page
            )
            .unwrap(),
            JsValue::String("post:multipart/form-data:multipart/form-data:/submit".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const form = document.querySelector('#default'); form.method = 'wat'; form.enctype = 'wat'; return form.method + ':' + form.enctype; })()",
                &reflected_form_page
            )
            .unwrap(),
            JsValue::String("get:application/x-www-form-urlencoded".to_owned())
        );

        let validity_page = Page::from_html(
            "file:///dom-validity-host.html",
            "<form id='f'>\
                <input id='email' type='email' required value='bad'>\
                <input id='age' type='number' min='10' max='20' step='2' value='13'>\
                <select id='plan' required><option value=''>pick</option><option value='pro' selected>Pro</option></select>\
                <textarea id='notes' readonly>ok</textarea>\
             </form>",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#email').willValidate",
                &validity_page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const v = document.querySelector('#email').validity; return (v instanceof ValidityState) + ':' + v.typeMismatch + ':' + v.valid; })()",
                &validity_page,
            )
            .unwrap(),
            JsValue::String("true:true:false".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#age').validity.stepMismatch",
                &validity_page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#plan').checkValidity()",
                &validity_page
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#notes').willValidate",
                &validity_page
            )
            .unwrap(),
            JsValue::Bool(false)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.querySelector('#f').checkValidity()",
                &validity_page
            )
            .unwrap(),
            JsValue::Bool(false)
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const input = document.querySelector('#email'); input.value = 'ada@example.test'; input.setCustomValidity('nope'); const before = input.validity.customError; input.setCustomValidity(''); return before + ':' + input.checkValidity(); })()",
                &validity_page,
            )
            .unwrap(),
            JsValue::String("true:true".to_owned())
        );

        let traversal_page = Page::from_html(
            "file:///dom-traversal-host.html",
            "<main><div id='walk-root'><article id='art-1'><p id='para-1'>one</p></article><aside id='aside-1'></aside></div></main>",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate_with_page(
                "document.createTreeWalker(document.getElementById('walk-root'), NodeFilter.SHOW_ELEMENT).firstChild().id",
                &traversal_page
            )
            .unwrap(),
            JsValue::String("art-1".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "document.createNodeIterator(document.getElementById('walk-root'), NodeFilter.SHOW_ELEMENT).nextNode().id",
                &traversal_page
            )
            .unwrap(),
            JsValue::String("art-1".to_owned())
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

    #[test]
    fn console_events_drain_from_current_realm() {
        let mut rt = JsRuntime::new().expect("engine init");

        assert_eq!(rt.drain_console_events().unwrap(), Vec::new());
        assert_eq!(
            rt.evaluate("console.log('hello', 7, true); 'done'")
                .unwrap(),
            JsValue::String("done".to_owned())
        );
        let events = rt.drain_console_events().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "log");
        assert_eq!(
            events[0].args[0].value,
            Some(JsConsoleValue::String("hello".into()))
        );
        assert_eq!(events[0].args[1].value, Some(JsConsoleValue::Number(7.0)));
        assert_eq!(events[0].args[2].value, Some(JsConsoleValue::Bool(true)));
        assert_eq!(rt.drain_console_events().unwrap(), Vec::new());
    }

    #[test]
    fn eval_persists_global_and_storage_state() {
        let mut rt = JsRuntime::new().expect("engine init");

        assert_eq!(
            rt.evaluate("globalThis.__vixenPersist = 41").unwrap(),
            JsValue::Int32(41)
        );
        assert_eq!(
            rt.evaluate("__vixenPersist + 1").unwrap(),
            JsValue::Int32(42)
        );
        assert_eq!(
            rt.evaluate("localStorage.setItem('persist', 'yes'); 'stored'")
                .unwrap(),
            JsValue::String("stored".to_owned())
        );
        assert_eq!(
            rt.evaluate("localStorage.getItem('persist')").unwrap(),
            JsValue::String("yes".to_owned())
        );
    }

    #[test]
    fn local_storage_round_trips_through_store_partitions() {
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-storage-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);

        {
            let mut rt = JsRuntime::with_storage_path(&path).expect("engine init");
            let page = Page::from_html("https://store.test/one", "<p>one</p>").unwrap();
            assert_eq!(
                rt.evaluate_with_page(
                    "localStorage.setItem('persist', 'yes'); sessionStorage.setItem('tab', 'one'); 'stored'",
                    &page,
                )
                .unwrap(),
                JsValue::String("stored".to_owned())
            );
        }

        {
            let mut rt = JsRuntime::with_storage_path(&path).expect("engine init");
            let same_origin = Page::from_html("https://store.test/two", "<p>two</p>").unwrap();
            assert_eq!(
                rt.evaluate_with_page(
                    "localStorage.getItem('persist') + ':' + sessionStorage.getItem('tab')",
                    &same_origin,
                )
                .unwrap(),
                JsValue::String("yes:one".to_owned())
            );

            let other_origin = Page::from_html("https://other.test/", "<p>other</p>").unwrap();
            assert_eq!(
                rt.evaluate_with_page("localStorage.getItem('persist')", &other_origin)
                    .unwrap(),
                JsValue::Null
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn page_eval_persists_until_page_snapshot_changes() {
        let mut rt = JsRuntime::new().expect("engine init");
        let page_one = Page::from_html(
            "file:///persist-one.html",
            "<html><head><title>One</title></head><body><p>first</p></body></html>",
        )
        .unwrap();
        let page_two = Page::from_html(
            "file:///persist-two.html",
            "<html><head><title>Two</title></head><body><p>second</p></body></html>",
        )
        .unwrap();

        assert_eq!(
            rt.evaluate_with_page(
                "globalThis.__pageTitle = document.title; localStorage.setItem('page', 'one'); __pageTitle",
                &page_one,
            )
            .unwrap(),
            JsValue::String("One".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "__pageTitle + ':' + localStorage.getItem('page') + ':' + document.title",
                &page_one,
            )
            .unwrap(),
            JsValue::String("One:one:One".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "typeof __pageTitle + ':' + localStorage.getItem('page') + ':' + document.title",
                &page_two,
            )
            .unwrap(),
            JsValue::String("undefined:null:Two".to_owned())
        );
        assert_eq!(
            rt.evaluate("typeof document").unwrap(),
            JsValue::String("undefined".to_owned())
        );
    }

    #[test]
    fn page_text_content_mutation_updates_page_facade_and_paint_inputs() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///mutate.html",
            "<html><head><style>body { margin: 0; } #status { display: block; width: 200px; height: 30px; }</style></head><body><p id='status'>waiting</p></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "const s = document.querySelector('#status'); s.textContent = 'clicked'; s.textContent",
                &mut page,
            )
            .unwrap();

        assert_eq!(value, JsValue::String("clicked".to_owned()));
        assert_eq!(page.text_content(), "clicked");
        assert_eq!(
            page.query_selector_all("#status").unwrap()[0].text,
            "clicked"
        );
        assert!(page.dump_lines((200, 100)).contains("clicked"));
        assert!(page.dump_display_list((200, 100)).contains("clicked"));
    }

    #[test]
    fn page_attribute_style_and_structural_mutations_update_page_dom() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///structural-mutate.html",
            "<html><head><style>body { margin: 0; } #status { display: inline; }</style></head><body><div id='container'><span id='keep'>keep</span></div><p id='status'>waiting</p></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "const status = document.querySelector('#status');\
                 status.setAttribute('data-state', 'ready');\
                 status.classList.add('active');\
                 status.style.display = 'block';\
                 status.style.width = '120px';\
                 const container = document.querySelector('#container');\
                 const made = document.createElement('div');\
                 made.id = 'made';\
                 made.className = 'chip';\
                 made.textContent = 'made';\
                 container.appendChild(made);\
                 const gone = document.createElement('em');\
                 gone.id = 'gone';\
                 gone.textContent = 'gone';\
                 container.appendChild(gone);\
                 container.removeChild(gone);\
                 const replacement = document.createElement('p');\
                 replacement.id = 'replacement';\
                 replacement.textContent = 'fresh';\
                 container.replaceChildren(made, replacement, ' tail');\
                 document.querySelector('#made').textContent + ':' +\
                   document.querySelector('#replacement').textContent + ':' +\
                   (document.querySelector('#gone') === null)",
                &mut page,
            )
            .unwrap();

        assert_eq!(value, JsValue::String("made:fresh:true".to_owned()));
        let status = &page.query_selector_all("#status").unwrap()[0];
        assert!(
            status
                .attributes
                .iter()
                .any(|(name, value)| name == "data-state" && value == "ready")
        );
        assert!(status.classes.iter().any(|class| class == "active"));
        let status_id = page.query_selector_all("#status").unwrap()[0].node_id;
        let computed = page.computed_style(status_id);
        assert!(computed.contains(&("display".to_owned(), "block".to_owned())));
        assert!(computed.contains(&("width".to_owned(), "120px".to_owned())));
        assert_eq!(page.query_selector_all("#made").unwrap().len(), 1);
        assert_eq!(page.query_selector_all("#replacement").unwrap().len(), 1);
        assert!(page.query_selector_all("#gone").unwrap().is_empty());
        let lines = page.dump_lines((240, 120));
        assert!(page.text_content().contains("made"));
        assert!(lines.contains("fresh"));
        assert!(lines.contains("tail"));
    }

    #[test]
    fn page_dynamic_inline_script_elements_execute_when_inserted() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///dynamic-script.html",
            "<html><body><main id='host'></main></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "const script = document.createElement('script');
                 script.text = \"document.body.setAttribute('data-script-ran', 'yes'); globalThis.__dynamicScriptRan = 12;\";
                 document.body.appendChild(script);
                 document.body.getAttribute('data-script-ran') + ':' + globalThis.__dynamicScriptRan",
                &mut page,
            )
            .unwrap();

        assert_eq!(value, JsValue::String("yes:12".to_owned()));
        assert_eq!(page.query_selector_all("script").unwrap().len(), 1);
        assert!(
            page.query_selector_all("body").unwrap()[0]
                .attributes
                .iter()
                .any(|(name, value)| name == "data-script-ran" && value == "yes")
        );
    }

    #[test]
    fn page_dynamic_style_elements_expose_sheet_and_update_cascade() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///dynamic-style.html",
            "<html><head></head><body><div id='target'>target</div></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "const style = document.createElement('style');
                 style.textContent = '#target { display: block; width: 123px; }';
                 let loaded = false;
                 style.onload = () => { loaded = true; };
                 document.head.appendChild(style);
                 String(!!style.sheet) + ':' + loaded + ':' + getComputedStyle(document.querySelector('#target')).width",
                &mut page,
            )
            .unwrap();

        assert_eq!(value, JsValue::String("true:true:123px".to_owned()));
        let target = page.query_selector_all("#target").unwrap()[0].node_id;
        let computed = page.computed_style(target);
        assert!(computed.contains(&("width".to_owned(), "123px".to_owned())));
    }

    #[test]
    fn page_mutation_observer_and_event_defaults_run_in_page_realm() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///observer-events.html",
            "<html><body><form id='form'><div id='parent'><input id='check' type='checkbox' name='agree'><button id='submit'>Send</button></div></form></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "new Promise((resolve) => {\
                   const order = [];\
                   const recordsSeen = [];\
                   const parent = document.querySelector('#parent');\
                   const check = document.querySelector('#check');\
                   const form = document.querySelector('#form');\
                   document.addEventListener('click', () => order.push('document-capture'), true);\
                   document.body.addEventListener('click', () => order.push('body-capture'), true);\
                   parent.addEventListener('click', () => order.push('parent-capture'), true);\
                   parent.addEventListener('click', () => order.push('parent-bubble'));\
                   check.addEventListener('click', () => order.push('target'));\
                   check.addEventListener('change', () => order.push('change'));\
                   form.addEventListener('submit', (event) => { order.push('submit'); event.preventDefault(); });\
                   const observer = new MutationObserver((records) => {\
                     for (const record of records) recordsSeen.push(record.type + ':' + (record.attributeName || '') + ':' + record.target.id);\
                     resolve(order.join('>') + '|' + check.checked + '|' + recordsSeen.join(',') + '|' + String(globalThis.__vixenLastFormSubmit));\
                   });\
                   observer.observe(document.body, { attributes: true, childList: true, subtree: true, attributeOldValue: true });\
                   check.click();\
                   document.querySelector('#submit').click();\
                 })",
                &mut page,
            )
            .unwrap();

        assert_eq!(
            value,
            JsValue::String(
                "document-capture>body-capture>parent-capture>target>parent-bubble>change>document-capture>body-capture>parent-capture>parent-bubble>submit|true|attributes:checked:check|undefined"
                    .to_owned()
            )
        );
        assert!(
            page.query_selector_all("#check").unwrap()[0]
                .attributes
                .iter()
                .any(|(name, _)| name == "checked")
        );

        let value = rt
            .evaluate_with_page_mut(
                "(() => {\
                   const check = document.querySelector('#check');\
                   check.checked = false;\
                   check.addEventListener('click', (event) => event.preventDefault(), { once: true });\
                   const returned = check.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, composed: true }));\
                   return returned + ':' + check.checked;\
                 })()",
                &mut page,
            )
            .unwrap();
        assert_eq!(value, JsValue::String("false:false".to_owned()));
    }

    #[test]
    fn page_editable_controls_update_value_selection_events_and_form_data() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///editable-controls.html",
            "<html><body><div id='dynamic-root'></div><form id='form' action='submit.html'><input id='name' name='name' value='Ada'><textarea id='body' name='body'>Hello</textarea><button id='go'>Go</button></form></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "(() => {\
                   const input = document.querySelector('#name');\
                   const body = document.querySelector('#body');\
                   const events = [];\
                   const dynamic = document.createElement('span');\
                   dynamic.id = 'inserted-before-form';\
                   dynamic.textContent = 'inserted';\
                   document.querySelector('#dynamic-root').replaceChildren(dynamic);\
                   input.addEventListener('keydown', (event) => events.push('keydown:' + event.key));\
                   input.addEventListener('input', (event) => events.push('input:' + event.inputType + ':' + event.data));\
                   input.addEventListener('change', () => events.push('change'));\
                   input.addEventListener('keyup', (event) => events.push('keyup:' + event.key));\
                   input.focus();\
                   input.select();\
                   globalThis.__vixenDispatchKeyEvent('keyDown', { key: 'Z', code: 'KeyZ', text: 'Z', inputText: 'Z', applyText: true });\
                   globalThis.__vixenDispatchKeyEvent('keyUp', { key: 'Z', code: 'KeyZ' });\
                   body.value = 'Typed body';\
                   return input.value + '|' + input.selectionStart + '|' + input.selectionEnd + '|' +\
                     new FormData(document.querySelector('#form')).get('name') + '|' +\
                     new FormData(document.querySelector('#form')).get('body') + '|' + events.join('>');\
                 })()",
                &mut page,
            )
            .unwrap();

        assert_eq!(
            value,
            JsValue::String(
                "Z|1|1|Z|Typed body|keydown:Z>input:insertText:Z>change>keyup:Z".to_owned()
            )
        );
        let submission = page.form_submission("form").unwrap();
        assert_eq!(
            String::from_utf8(submission.body).unwrap(),
            "name=Z&body=Typed+body"
        );
    }

    #[test]
    fn page_navigation_actions_drain_from_page_realm() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///nav/index.html",
            "<html><body><a id='next' href='next.html'>Next</a><form id='form' action='submit.html'><button id='go'>Go</button></form></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "history.pushState({ ok: 1 }, 'title', 'state.html');\
                 document.querySelector('#next').click();\
                 document.querySelector('#go').click();\
                 history.length + ':' + history.state.ok + ':' + location.href",
                &mut page,
            )
            .unwrap();
        assert_eq!(
            value,
            JsValue::String("2:1:file:///nav/state.html".to_owned())
        );

        assert_eq!(
            rt.drain_navigation_actions().unwrap(),
            vec![
                JsNavigationAction::HistoryPush {
                    url: "file:///nav/state.html".to_owned(),
                    state_json: r#"{"ok":1}"#.to_owned(),
                    title: "title".to_owned(),
                },
                JsNavigationAction::Navigate {
                    url: "file:///nav/next.html".to_owned(),
                    replace: false,
                },
                JsNavigationAction::FormSubmit {
                    form_id: "form".to_owned(),
                    form_node_id: page.query_selector_all("#form").unwrap()[0].node_id,
                    submitter_id: Some("go".to_owned()),
                    action: "file:///nav/submit.html".to_owned(),
                    method: "get".to_owned(),
                },
            ]
        );
        assert_eq!(rt.drain_navigation_actions().unwrap(), Vec::new());
    }

    #[test]
    fn page_history_accessors_use_page_realm() {
        let mut rt = JsRuntime::new().expect("engine init");
        let page = Page::from_html(
            "file:///nav/initial.html",
            "<html><body><p>history accessors</p></body></html>",
        )
        .unwrap();

        assert_eq!(
            rt.evaluate_with_page(
                "history.length + ':' + window.history.length + ':' + history.state + ':' + history.scrollRestoration",
                &page,
            )
            .unwrap(),
            JsValue::String("1:1:null:auto".to_owned())
        );
    }

    #[test]
    fn page_inline_scripts_run_in_page_realm_and_honor_csp() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "https://example.com/inline.html",
            "<html><head><title>Inline</title></head><body>\
             <script>globalThis.__inlineCount = 40; localStorage.setItem('inline', 'ran');</script>\
             <script>globalThis.__inlineCount += 2;</script>\
             </body></html>",
        )
        .unwrap();

        assert_eq!(rt.execute_page_scripts(&mut page).unwrap(), 2);
        assert_eq!(
            rt.evaluate_with_page(
                "__inlineCount + ':' + localStorage.getItem('inline') + ':' + document.title",
                &page,
            )
            .unwrap(),
            JsValue::String("42:ran:Inline".to_owned())
        );

        let mut blocked = Page::from_html(
            "https://example.com/blocked.html",
            "<meta http-equiv='Content-Security-Policy' content=\"script-src 'self'\">\
             <script>globalThis.__blockedInline = true;</script>",
        )
        .unwrap();
        assert_eq!(rt.execute_page_scripts(&mut blocked).unwrap(), 0);
        assert_eq!(
            rt.evaluate_with_page("typeof __blockedInline", &blocked)
                .unwrap(),
            JsValue::String("undefined".to_owned())
        );

        let mut header_blocked = Page::from_html_with_headers(
            "https://example.com/header-blocked.html",
            "<script>globalThis.__headerBlockedInline = true;</script>",
            [("Content-Security-Policy", "script-src 'self'")],
        )
        .unwrap();
        assert_eq!(rt.execute_page_scripts(&mut header_blocked).unwrap(), 0);
        assert_eq!(
            rt.evaluate_with_page("typeof __headerBlockedInline", &header_blocked)
                .unwrap(),
            JsValue::String("undefined".to_owned())
        );
    }

    #[test]
    fn page_external_scripts_fetch_execute_and_fail_closed() {
        let (base_url, network_config, server) = spawn_script_server(
            "vixen-script-success.com",
            "globalThis.__externalOrder += ':external'; localStorage.setItem('external-script', 'ran');",
            &[("Content-Type", "text/javascript; charset=utf-8")],
        );
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let html = format!(
            "<base href='{base_url}/assets/'>\
             <script>globalThis.__externalOrder = 'inline';</script>\
             <script src='app.js'></script>"
        );
        let mut page = Page::from_html(format!("{base_url}/page.html"), &html).unwrap();

        assert_eq!(rt.execute_page_scripts(&mut page).unwrap(), 2);
        assert_eq!(
            rt.evaluate_with_page(
                "__externalOrder + ':' + localStorage.getItem('external-script')",
                &page,
            )
            .unwrap(),
            JsValue::String("inline:external:ran".to_owned())
        );
        server.join().unwrap();

        let (base_url, network_config, server) = spawn_script_server(
            "vixen-script-nonce.com",
            "globalThis.__externalNonceRan = true;",
            &[("Content-Type", "text/javascript")],
        );
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut page = Page::from_html(
            format!("{base_url}/page.html"),
            "<meta http-equiv='Content-Security-Policy' content=\"script-src 'nonce-ext'\">\
             <script src='/nonce.js' nonce='ext'></script>",
        )
        .unwrap();
        assert_eq!(rt.execute_page_scripts(&mut page).unwrap(), 1);
        assert_eq!(
            rt.evaluate_with_page("__externalNonceRan", &page).unwrap(),
            JsValue::Bool(true)
        );
        server.join().unwrap();

        let mut rt = JsRuntime::new().expect("engine init");
        let mut nonce_blocked = Page::from_html(
            "https://example.com/nonce-blocked.html",
            "<meta http-equiv='Content-Security-Policy' content=\"script-src 'nonce-ext'\">\
             <script src='https://cdn.example/app.js' nonce='wrong'></script>",
        )
        .unwrap();
        assert_eq!(rt.execute_page_scripts(&mut nonce_blocked).unwrap(), 0);
        assert_eq!(
            rt.evaluate_with_page("typeof __externalNonceBlocked", &nonce_blocked)
                .unwrap(),
            JsValue::String("undefined".to_owned())
        );

        let mut rt = JsRuntime::new().expect("engine init");
        let mut csp_blocked = Page::from_html(
            "https://example.com/csp-blocked.html",
            "<meta http-equiv='Content-Security-Policy' content=\"script-src 'self'\">\
             <script src='https://cdn.example/app.js'></script>",
        )
        .unwrap();
        assert_eq!(rt.execute_page_scripts(&mut csp_blocked).unwrap(), 0);
        assert_eq!(
            rt.evaluate_with_page("typeof __externalCspBlocked", &csp_blocked)
                .unwrap(),
            JsValue::String("undefined".to_owned())
        );

        let mut policy_blocked = Page::from_html(
            "http://vixen-url-policy.com/page.html",
            "<script src='http://127.0.0.1:9/app.js'></script>",
        )
        .unwrap();
        assert_eq!(rt.execute_page_scripts(&mut policy_blocked).unwrap(), 0);
        assert_eq!(
            rt.evaluate_with_page("typeof __externalPolicyBlocked", &policy_blocked)
                .unwrap(),
            JsValue::String("undefined".to_owned())
        );

        let (base_url, network_config, server) = spawn_script_server(
            "vixen-script-nosniff.com",
            "globalThis.__externalNosniffBlocked = true;",
            &[
                ("Content-Type", "text/plain"),
                ("X-Content-Type-Options", "nosniff"),
            ],
        );
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut page = Page::from_html(
            format!("{base_url}/page.html"),
            "<script src='/blocked.js'></script>",
        )
        .unwrap();
        assert_eq!(rt.execute_page_scripts(&mut page).unwrap(), 0);
        assert_eq!(
            rt.evaluate_with_page("typeof __externalNosniffBlocked", &page)
                .unwrap(),
            JsValue::String("undefined".to_owned())
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_returns_http_response() {
        let (url, network_config, server) =
            spawn_fetch_server("vixen-fetch-success.com", "hello fetch");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        assert_eq!(
            rt.evaluate("globalThis.__beforeFetch = 7").unwrap(),
            JsValue::Int32(7)
        );
        let expr = format!(
            "fetch({url:?}).then((response) => response.text().then((body) => response.status + ':' + response.url + ':' + response.headers.get('x-vixen-test') + ':' + body))"
        );

        assert_eq!(
            rt.evaluate(&expr).unwrap(),
            JsValue::String(format!("200:{url}:yes:hello fetch"))
        );
        server.join().unwrap();
        assert_eq!(rt.evaluate("__beforeFetch + 1").unwrap(), JsValue::Int32(8));
    }

    #[test]
    fn fetch_blocks_private_hosts() {
        let mut rt = JsRuntime::new().expect("engine init");

        assert_eq!(
            rt.evaluate("fetch('http://127.0.0.1:9/').then(() => false, (err) => err instanceof TypeError && /blocked host/.test(err.message))")
                .unwrap(),
            JsValue::Bool(true)
        );
    }

    fn webidl_all_constructors_expr() -> String {
        let names = webidl::manifest_interface_names();
        let names = deno_core::serde_json::to_string(&names).unwrap();
        format!(
            "(() => {{ const names = {names}; const isExposed = (name) => typeof globalThis[name] === 'function' || (name === 'CSS' && typeof globalThis[name] === 'object' && globalThis.CSS !== null); return globalThis.__vixenWebidl.interfaceNames().length === names.length && names.every((name) => typeof globalThis.__vixenWebidl.interfaceConstructor(name) === 'function' && isExposed(name)); }})()"
        )
    }

    fn webidl_parent_chains_expr() -> String {
        let pairs = webidl::manifest_parent_pairs()
            .into_iter()
            .map(|(name, parent)| [name, parent])
            .collect::<Vec<_>>();
        let pairs = deno_core::serde_json::to_string(&pairs).unwrap();
        format!(
            "(() => {{ const pairs = {pairs}; return pairs.every(([name, parent]) => globalThis[name].prototype instanceof globalThis[parent]); }})()"
        )
    }
}
