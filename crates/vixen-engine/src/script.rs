//! `deno_core` runtime — the script execution boundary.
//!
//! The public Vixen-facing seam stays small (`JsRuntime`, `JsValue`, eval
//! methods), but the implementation uses `deno_core`/V8 directly per ADR-014.
//! Host surfaces are installed from focused bootstrap modules before the caller's
//! script runs. A `JsRuntime` owns a persistent V8 realm: sequential evals share
//! globals, storage host state, pending microtasks, and network host state until
//! the caller switches between the page and non-page realms or navigates to a new
//! page snapshot.

#![deny(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
use std::io::Read;

use crate::doc::DocumentScriptItem;
use crate::engine_error::{EngineError, codes};
#[cfg(test)]
use crate::mime::MimeType;
use crate::page::Page;
use crate::storage_key::{StorageKind, StoragePartition};

mod cssom;
mod dom;
mod encoding;
mod import_maps;
mod module_loader;
mod runtime;
mod webapi;
mod webidl;

pub use runtime::RenderLayoutCancellation;
pub(crate) use runtime::RuntimeInterruptHandle;

/// Vixen's JavaScript runtime seam, backed by `deno_core`/V8.
pub struct JsRuntime {
    network_config: vixen_net::NetworkConfig,
    storage_backend: webapi::WebStorageBackend,
    storage_temp_path: Option<PathBuf>,
    storage_session_id: String,
    storage_opaque_serial: u64,
    record_visits_on_realm: bool,
    extra_http_headers: webapi::ExtraHttpHeaders,
    cache_disabled: webapi::CacheDisabledFlag,
    permission_overrides: webapi::PermissionOverrides,
    runtime_network_state: webapi::RuntimeNetworkState,
    runtime_interrupt: RuntimeInterruptHandle,
    module_loader: module_loader::PageModuleLoader,
    event_loop_executor: Option<tokio::runtime::Runtime>,
    runtime: Option<deno_core::JsRuntime>,
    dom_mutations: Option<dom::DomMutationSink>,
    synchronous_layout: Option<SynchronousLayoutConfig>,
    realm_key: RealmKey,
    cancelled_module_realm: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RenderViewState {
    pub viewport: (u32, u32),
    pub viewport_generation: u64,
    pub device_scale: f64,
    pub page_zoom: f64,
}

#[derive(Clone)]
pub(crate) struct SynchronousLayoutConfig {
    page: Rc<RefCell<Page>>,
    context_id: vixen_api::BrowsingContextId,
    document_id: vixen_api::DocumentId,
    view: Rc<Cell<RenderViewState>>,
    renderer: std::sync::Arc<dyn crate::browser::SynchronousRenderer>,
}

struct JsRuntimeStorageConfig<'a> {
    storage_temp_path: Option<PathBuf>,
    storage_session_id: Option<String>,
    record_visits_on_realm: bool,
    initial_page: Option<&'a Page>,
    synchronous_layout: Option<SynchronousLayoutConfig>,
    module_request_ids: Option<crate::browser::SharedRequestIdAllocator>,
    module_executor: Option<std::sync::Arc<tokio::runtime::Runtime>>,
}

impl SynchronousLayoutConfig {
    pub(crate) fn new(
        page: Rc<RefCell<Page>>,
        context_id: vixen_api::BrowsingContextId,
        document_id: vixen_api::DocumentId,
        view: Rc<Cell<RenderViewState>>,
        renderer: std::sync::Arc<dyn crate::browser::SynchronousRenderer>,
    ) -> Self {
        Self {
            page,
            context_id,
            document_id,
            view,
            renderer,
        }
    }
}

/// Persistent parser-discovered script state advanced one item at a time.
pub(crate) struct PageScriptRunner {
    items: std::vec::IntoIter<DocumentScriptItem>,
    deferred_modules: std::collections::VecDeque<PreparedPageScript>,
    csp: vixen_net::csp::ContentSecurityPolicy,
    origin: vixen_net::Origin,
    bypass_csp: bool,
    next_inline_classic: u64,
    next_inline_module: u64,
    import_map: Option<import_maps::PageImportMap>,
    import_map_seen: bool,
    module_seen: bool,
}

pub(crate) enum PreparedPageScript {
    Skip,
    Inline {
        source: String,
        request: Box<ExternalPageScript>,
    },
    InlineModule {
        source: String,
        request: Box<ExternalPageScript>,
    },
    ImportMap {
        diagnostics: Vec<String>,
        error: Option<String>,
    },
    External(Box<ExternalPageScript>),
}

#[derive(Clone)]
pub(crate) struct ExternalPageScript {
    url: url::Url,
    csp: Option<vixen_net::csp::ContentSecurityPolicy>,
    origin: vixen_net::Origin,
    nonce: Option<String>,
    context_trustworthy: bool,
    module: bool,
    module_credentials: ModuleCredentialsMode,
    import_map: Option<import_maps::PageImportMap>,
    integrity: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModuleCredentialsMode {
    SameOrigin,
    Include,
}

impl ExternalPageScript {
    pub(crate) fn url(&self) -> &url::Url {
        &self.url
    }

    pub(crate) fn allows_url(&self, url: &url::Url) -> bool {
        self.blocked_reason(url).is_none()
    }

    pub(crate) fn blocked_reason(&self, url: &url::Url) -> Option<&'static str> {
        if self.csp.as_ref().is_some_and(|csp| {
            !csp.allows_external_script(&self.origin, url, self.nonce.as_deref())
        }) {
            return Some("csp");
        }
        if matches!(
            vixen_net::classify_mixed_content(
                self.context_trustworthy,
                url,
                vixen_net::ResourceType::Script,
                false,
            ),
            vixen_net::MixedContentVerdict::Block
        ) {
            return Some("mixed-content");
        }
        None
    }

    pub(crate) fn is_cross_site(&self, url: &url::Url) -> bool {
        !vixen_net::is_same_site(&self.origin, &vixen_net::Origin::from_url(url))
    }

    pub(crate) fn request_headers(&self, url: &url::Url) -> Vec<(String, String)> {
        if self.uses_cors() && self.is_cross_origin(url) {
            vec![("origin".to_owned(), cors_origin_value(&self.origin))]
        } else {
            Vec::new()
        }
    }

    pub(crate) fn sends_credentials(&self, url: &url::Url) -> bool {
        !self.uses_cors()
            || !self.is_cross_origin(url)
            || self.module_credentials == ModuleCredentialsMode::Include
    }

    pub(crate) fn response_blocked_reason(
        &self,
        url: &url::Url,
        response: &vixen_net::ByteResponse,
    ) -> Option<&'static str> {
        if !self.uses_cors() || !self.is_cross_origin(url) {
            return None;
        }
        let headers = vixen_net::CorsResponseHeaders::from_headers(response.headers.iter());
        let credentials = match self.module_credentials {
            ModuleCredentialsMode::SameOrigin => vixen_net::CorsCredentialsMode::SameOrigin,
            ModuleCredentialsMode::Include => vixen_net::CorsCredentialsMode::Include,
        };
        matches!(
            vixen_net::cors_check(&headers, &cors_origin_value(&self.origin), credentials),
            vixen_net::CorsCheckOutcome::Fail(_)
        )
        .then_some("cors")
    }

    pub(crate) fn is_module(&self) -> bool {
        self.module
    }

    pub(crate) fn integrity_failure(&self, body: &[u8]) -> Option<String> {
        let metadata = self
            .integrity
            .as_deref()
            .filter(|value| !value.is_empty())?;
        let items = vixen_net::parse_integrity(metadata);
        match vixen_net::verify_integrity(&items, body) {
            vixen_net::IntegrityOutcome::Mismatch(algorithms) => Some(format!(
                "external script blocked by integrity mismatch ({})",
                algorithms
                    .into_iter()
                    .map(vixen_net::HashAlgorithm::token)
                    .collect::<Vec<_>>()
                    .join(",")
            )),
            vixen_net::IntegrityOutcome::NoMetadata
            | vixen_net::IntegrityOutcome::NoKnownAlgorithms
            | vixen_net::IntegrityOutcome::Verified(_) => None,
        }
    }

    pub(crate) fn with_url(&self, url: url::Url) -> Self {
        let mut request = self.clone();
        request.url = url;
        request
    }

    fn import_map(&self) -> Option<import_maps::PageImportMap> {
        self.import_map.clone()
    }

    fn dynamic_import_root(&self) -> Self {
        let mut request = self.clone();
        request.module = true;
        request.module_credentials = ModuleCredentialsMode::SameOrigin;
        request
    }

    fn automation(
        page: &Page,
        bypass_csp: bool,
        import_map: Option<import_maps::PageImportMap>,
    ) -> Option<Self> {
        let mut url = url::Url::parse(&page.document_base_uri())
            .or_else(|_| url::Url::parse(page.url()))
            .ok()?;
        url.set_fragment(Some("vixen-automation"));
        Some(Self {
            url,
            csp: (!bypass_csp).then(|| page.csp().clone()),
            origin: page_origin(page),
            nonce: None,
            context_trustworthy: url::Url::parse(page.url())
                .ok()
                .as_ref()
                .is_some_and(vixen_net::referrer_policy::is_potentially_trustworthy),
            module: true,
            module_credentials: ModuleCredentialsMode::SameOrigin,
            import_map,
            integrity: None,
        })
    }

    fn module_dependency(&self, url: url::Url) -> Result<Self, &'static str> {
        if !self.module {
            return Err("module-policy");
        }
        if !matches!(url.scheme(), "file" | "http" | "https")
            || matches!(self.origin.scheme(), "http" | "https") && url.scheme() == "file"
        {
            return Err("module-policy");
        }
        let request = Self {
            url,
            csp: self.csp.clone(),
            origin: self.origin.clone(),
            nonce: None,
            context_trustworthy: self.context_trustworthy,
            module: true,
            module_credentials: self.module_credentials,
            import_map: self.import_map.clone(),
            integrity: None,
        };
        if let Some(reason) = request.blocked_reason(&request.url) {
            return Err(reason);
        }
        Ok(request)
    }

    fn is_cross_origin(&self, url: &url::Url) -> bool {
        vixen_net::Origin::from_url(url) != self.origin
    }

    fn uses_cors(&self) -> bool {
        self.module
            || self
                .integrity
                .as_deref()
                .is_some_and(|value| !value.is_empty())
    }
}

fn cors_origin_value(origin: &vixen_net::Origin) -> String {
    if origin.is_opaque() {
        return "null".to_owned();
    }
    match (origin.scheme(), origin.host(), origin.port()) {
        ("http", host, Some(80)) | ("https", host, Some(443)) => {
            format!("{}://{host}", origin.scheme())
        }
        (_, host, Some(port)) => format!("{}://{host}:{port}", origin.scheme()),
        (_, host, None) => format!("{}://{host}", origin.scheme()),
    }
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

/// A stable network lifecycle event captured from fetch() in the current realm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsNetworkEvent {
    Request {
        request_id: String,
        url: String,
        method: String,
    },
    Redirect {
        request_id: String,
        from: String,
        to: String,
        status: u16,
    },
    Response {
        request_id: String,
        url: String,
        status: u16,
    },
    Progress {
        request_id: String,
        url: String,
        chunk_bytes: u64,
        loaded_bytes: u64,
        total_bytes: Option<u64>,
    },
    Completed {
        request_id: String,
        url: String,
        body_bytes: u64,
    },
    Failure {
        request_id: String,
        url: String,
        error_text: String,
        blocked_reason: Option<String>,
    },
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
        submitter_node_id: Option<usize>,
        submitter_id: Option<String>,
        action: String,
        method: String,
        enctype: String,
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
    HistoryScrollRestoration {
        value: String,
    },
    Overflow,
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
        Self::with_storage_backend(
            network_config,
            storage_backend,
            JsRuntimeStorageConfig {
                storage_temp_path,
                storage_session_id: None,
                record_visits_on_realm: true,
                initial_page: None,
                synchronous_layout: None,
                module_request_ids: None,
                module_executor: None,
            },
        )
    }

    /// Clone the transport policy used to construct this runtime. Transitional
    /// automation tests use this to seed BrowserCore without transferring V8
    /// ownership into the protocol adapter.
    pub fn network_config(&self) -> vixen_net::NetworkConfig {
        self.network_config.clone()
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
        Self::with_storage_backend(
            network_config,
            storage_backend,
            JsRuntimeStorageConfig {
                storage_temp_path: None,
                storage_session_id: None,
                record_visits_on_realm: true,
                initial_page: None,
                synchronous_layout: None,
                module_request_ids: None,
                module_executor: None,
            },
        )
    }

    /// Construct a context runtime over the Store opened once by BrowserCore.
    /// The explicit session id partitions `sessionStorage` by browsing context
    /// while same-origin `localStorage` remains profile shared.
    pub(crate) fn with_browser_storage(
        network_config: vixen_net::NetworkConfig,
        store: std::sync::Arc<vixen_store::Store>,
        storage_session_id: String,
        page: &Page,
        module_request_ids: crate::browser::SharedRequestIdAllocator,
        module_executor: std::sync::Arc<tokio::runtime::Runtime>,
    ) -> Result<Self, EngineError> {
        Self::with_storage_backend(
            network_config,
            webapi::WebStorageBackend::from_store(store),
            JsRuntimeStorageConfig {
                storage_temp_path: None,
                storage_session_id: Some(storage_session_id),
                record_visits_on_realm: false,
                initial_page: Some(page),
                synchronous_layout: None,
                module_request_ids: Some(module_request_ids),
                module_executor: Some(module_executor),
            },
        )
    }

    pub(crate) fn with_browser_storage_and_renderer(
        network_config: vixen_net::NetworkConfig,
        store: std::sync::Arc<vixen_store::Store>,
        storage_session_id: String,
        synchronous_layout: SynchronousLayoutConfig,
        module_request_ids: crate::browser::SharedRequestIdAllocator,
        module_executor: std::sync::Arc<tokio::runtime::Runtime>,
    ) -> Result<Self, EngineError> {
        let page = Rc::clone(&synchronous_layout.page);
        let page_ref = page.borrow();
        Self::with_storage_backend(
            network_config,
            webapi::WebStorageBackend::from_store(store),
            JsRuntimeStorageConfig {
                storage_temp_path: None,
                storage_session_id: Some(storage_session_id),
                record_visits_on_realm: false,
                initial_page: Some(&page_ref),
                synchronous_layout: Some(synchronous_layout),
                module_request_ids: Some(module_request_ids),
                module_executor: Some(module_executor),
            },
        )
    }

    fn with_storage_backend(
        network_config: vixen_net::NetworkConfig,
        storage_backend: webapi::WebStorageBackend,
        config: JsRuntimeStorageConfig<'_>,
    ) -> Result<Self, EngineError> {
        let JsRuntimeStorageConfig {
            storage_temp_path,
            storage_session_id,
            record_visits_on_realm,
            initial_page,
            synchronous_layout,
            module_request_ids,
            module_executor,
        } = config;
        let storage_session_id = storage_session_id.unwrap_or_else(next_storage_session_id);
        let storage_opaque_serial = 1;
        let extra_http_headers = webapi::ExtraHttpHeaders::default();
        let cache_disabled = webapi::CacheDisabledFlag::default();
        let permission_overrides = webapi::PermissionOverrides::default();
        let runtime_network_state = webapi::RuntimeNetworkState::default();
        let runtime_interrupt = RuntimeInterruptHandle::default();
        let module_loader = module_loader::PageModuleLoader::new(
            network_config.clone(),
            storage_backend.clone(),
            runtime_network_state.clone(),
            cache_disabled.clone(),
            module_request_ids,
            module_executor,
        );
        let event_loop_executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                EngineError::script(
                    codes::SCRIPT_EVAL,
                    format!("script event-loop executor unavailable: {error}"),
                )
            })?;
        let init = {
            let _executor_guard = event_loop_executor.enter();
            runtime::new_deno_runtime(
                initial_page,
                runtime::DenoRuntimeConfig {
                    network: network_config.clone(),
                    storage: web_storage_host(
                        initial_page,
                        &storage_backend,
                        &storage_session_id,
                        storage_opaque_serial,
                    ),
                    network_state: runtime_network_state.clone(),
                    extra_http_headers: extra_http_headers.clone(),
                    cache_disabled: cache_disabled.clone(),
                    permission_overrides: permission_overrides.clone(),
                    interrupt: runtime_interrupt.clone(),
                    synchronous_layout: synchronous_layout.clone(),
                    module_loader: module_loader.clone(),
                },
            )?
        };
        let realm_key = initial_page
            .map(page_realm_key)
            .map(RealmKey::Page)
            .unwrap_or(RealmKey::NoPage);
        Ok(Self {
            network_config,
            storage_backend,
            storage_temp_path,
            storage_session_id,
            storage_opaque_serial,
            record_visits_on_realm,
            extra_http_headers,
            cache_disabled,
            permission_overrides,
            runtime_network_state,
            runtime_interrupt,
            module_loader,
            event_loop_executor: Some(event_loop_executor),
            runtime: Some(init.runtime),
            dom_mutations: init.dom_mutations,
            synchronous_layout,
            realm_key,
            cancelled_module_realm: false,
        })
    }

    /// Run one operation with this runtime's V8 isolate entered on the current
    /// browser-core thread. rusty_v8 keeps the most recently constructed isolate
    /// entered for its lifetime, so a browser with multiple context isolates must
    /// temporarily enter older isolates before using them.
    ///
    /// The closure must not replace or drop this runtime. BrowserCore creates a
    /// fresh runtime slot for each cross-document generation and retires old slots
    /// in LIFO-safe order instead of calling `reset_realm` through this method.
    #[allow(unsafe_code)]
    pub(crate) fn with_entered_isolate<T>(&mut self, operation: impl FnOnce(&mut Self) -> T) -> T {
        struct ExitGuard(*mut deno_core::v8::OwnedIsolate);

        impl Drop for ExitGuard {
            #[allow(unsafe_code)]
            fn drop(&mut self) {
                // SAFETY: `with_entered_isolate` keeps the owning JsRuntime
                // alive for the guard's lifetime and balances exactly one enter.
                unsafe { (*self.0).exit() };
            }
        }

        let isolate = self
            .runtime
            .as_mut()
            .expect("browser runtime slot must be initialised")
            .v8_isolate() as *mut deno_core::v8::OwnedIsolate;
        // SAFETY: runtime slots never cross threads, the isolate remains alive
        // through `operation`, and ExitGuard restores the previously entered
        // isolate even if the operation unwinds.
        unsafe { (*isolate).enter() };
        let _exit = ExitGuard(isolate);
        operation(self)
    }

    pub(crate) fn interrupt_handle(&self) -> RuntimeInterruptHandle {
        self.runtime_interrupt.clone()
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
        let value = match self.evaluate_with_page_context(src, Some(&*page)) {
            Ok(value) => value,
            Err(error) => {
                self.discard_dom_mutations();
                return Err(error);
            }
        };
        if !self.apply_dom_mutations(page)? {
            return Ok(value);
        }
        const MAX_SCROLL_SYNC_ROUNDS: usize = 8;
        for _ in 0..MAX_SCROLL_SYNC_ROUNDS {
            let source = dom::element_scroll_state_source(page, true);
            if let Err(error) = self.evaluate_with_page_context(&source, Some(&*page)) {
                self.discard_dom_mutations();
                return Err(error);
            }
            if !self.apply_dom_mutations(page)? {
                return Ok(value);
            }
        }
        self.discard_dom_mutations();
        Err(EngineError::script(
            codes::SCRIPT_EVAL,
            "element scroll synchronization exceeded the mutation round limit",
        ))
    }

    #[cfg(test)]
    fn evaluate_module_with_page_mut(
        &mut self,
        source: &str,
        request: &ExternalPageScript,
        page: &mut Page,
    ) -> Result<(), EngineError> {
        self.ensure_realm(Some(&*page))?;
        if let Err(error) = self.execute_module_in_current_realm(source, request) {
            self.discard_dom_mutations();
            return Err(error);
        }
        if !self.apply_dom_mutations(page)? {
            return Ok(());
        }
        const MAX_SCROLL_SYNC_ROUNDS: usize = 8;
        for _ in 0..MAX_SCROLL_SYNC_ROUNDS {
            let source = dom::element_scroll_state_source(page, true);
            if let Err(error) = self.evaluate_with_page_context(&source, Some(&*page)) {
                self.discard_dom_mutations();
                return Err(error);
            }
            if !self.apply_dom_mutations(page)? {
                return Ok(());
            }
        }
        self.discard_dom_mutations();
        Err(EngineError::script(
            codes::SCRIPT_EVAL,
            "element scroll synchronization exceeded the mutation round limit",
        ))
    }

    pub(crate) fn evaluate_with_shared_page_mut(
        &mut self,
        src: &str,
        page: &Rc<RefCell<Page>>,
    ) -> Result<JsValue, EngineError> {
        let request = {
            let page = page.borrow();
            ExternalPageScript::automation(&page, false, self.module_loader.document_import_map())
        };
        self.evaluate_with_shared_page_mut_request(src, page, request.as_ref())
    }

    pub(crate) fn evaluate_for_automation_with_shared_page_mut(
        &mut self,
        src: &str,
        page: &Rc<RefCell<Page>>,
        bypass_csp: bool,
    ) -> Result<JsValue, EngineError> {
        let request = {
            let page = page.borrow();
            ExternalPageScript::automation(
                &page,
                bypass_csp,
                self.module_loader.document_import_map(),
            )
        };
        self.evaluate_with_shared_page_mut_request(src, page, request.as_ref())
    }

    fn evaluate_with_shared_page_mut_request(
        &mut self,
        src: &str,
        page: &Rc<RefCell<Page>>,
        request: Option<&ExternalPageScript>,
    ) -> Result<JsValue, EngineError> {
        self.ensure_realm(Some(&page.borrow()))?;
        let value = match self.execute_in_current_realm(src, request) {
            Ok(value) => value,
            Err(error) => {
                self.discard_dom_mutations();
                return Err(error);
            }
        };
        {
            let mut page = page.borrow_mut();
            self.apply_dom_mutations(&mut page)?;
            self.realm_key = RealmKey::Page(page_realm_key(&page));
        }

        const MAX_SCROLL_SYNC_ROUNDS: usize = 8;
        for _ in 0..MAX_SCROLL_SYNC_ROUNDS {
            let source = dom::element_scroll_state_source(&page.borrow(), true);
            let result = self.execute_in_current_realm(&source, request);
            if let Err(error) = result {
                self.discard_dom_mutations();
                return Err(error);
            }
            let mut page = page.borrow_mut();
            if !self.apply_dom_mutations(&mut page)? {
                self.realm_key = RealmKey::Page(page_realm_key(&page));
                return Ok(value);
            }
            self.realm_key = RealmKey::Page(page_realm_key(&page));
        }
        self.discard_dom_mutations();
        Err(EngineError::script(
            codes::SCRIPT_EVAL,
            "element scroll synchronization exceeded the mutation round limit",
        ))
    }

    pub(crate) fn evaluate_module_with_shared_page_mut(
        &mut self,
        source: &str,
        request: &ExternalPageScript,
        page: &Rc<RefCell<Page>>,
    ) -> Result<(), EngineError> {
        {
            let page = page.borrow();
            self.ensure_realm(Some(&page))?;
        }
        if let Err(error) = self.execute_module_in_current_realm(source, request) {
            self.discard_dom_mutations();
            return Err(error);
        }
        {
            let mut page = page.borrow_mut();
            self.apply_dom_mutations(&mut page)?;
            self.realm_key = RealmKey::Page(page_realm_key(&page));
        }

        const MAX_SCROLL_SYNC_ROUNDS: usize = 8;
        for _ in 0..MAX_SCROLL_SYNC_ROUNDS {
            let source = dom::element_scroll_state_source(&page.borrow(), true);
            if let Err(error) = self.execute_in_current_realm(&source, Some(request)) {
                self.discard_dom_mutations();
                return Err(error);
            }
            let mut page = page.borrow_mut();
            if !self.apply_dom_mutations(&mut page)? {
                self.realm_key = RealmKey::Page(page_realm_key(&page));
                return Ok(());
            }
            self.realm_key = RealmKey::Page(page_realm_key(&page));
        }
        self.discard_dom_mutations();
        Err(EngineError::script(
            codes::SCRIPT_EVAL,
            "element scroll synchronization exceeded the mutation round limit",
        ))
    }

    pub(crate) fn evaluate_classic_with_shared_page_mut(
        &mut self,
        source: &str,
        request: &ExternalPageScript,
        page: &Rc<RefCell<Page>>,
    ) -> Result<JsValue, EngineError> {
        self.evaluate_with_shared_page_mut_request(source, page, Some(request))
    }

    /// Set browser-controlled extra HTTP headers for subsequent runtime fetches.
    ///
    /// CDP owns validation before calling this; the lower `vixen-net` boundary
    /// validates again before bytes leave the process.
    pub fn set_extra_http_headers(&mut self, headers: Vec<(String, String)>) {
        self.extra_http_headers.set(headers);
    }

    /// Toggle browser HTTP cache use for subsequent runtime fetches.
    pub fn set_cache_disabled(&mut self, disabled: bool) {
        self.cache_disabled.set(disabled);
    }

    /// Replace the inspector permission grant set for one origin, or for the
    /// wildcard scope when `origin` is `None`. Permissions omitted from the set
    /// are denied for that scope, matching CDP `Browser.grantPermissions`.
    pub fn replace_permission_grants(&mut self, origin: Option<String>, grants: Vec<String>) {
        self.permission_overrides.replace(origin, grants);
    }

    /// Clear all inspector permission overrides without changing persisted
    /// profile decisions.
    pub fn reset_permission_overrides(&mut self) {
        self.permission_overrides.reset();
    }

    /// Drop runtime-local network state after profile data is cleared so an
    /// active realm cannot repopulate deleted cookies or preflight decisions.
    pub(crate) fn clear_profile_network_state(&self, cookies: bool, fetch_cache: bool) {
        self.runtime_network_state.clear(cookies, fetch_cache);
    }

    pub(crate) fn network_cookie_snapshots(
        &self,
    ) -> Result<Vec<vixen_net::CookieSnapshot>, EngineError> {
        self.runtime_network_state
            .cookie_snapshots()
            .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))
    }

    pub(crate) fn apply_network_cookie_delta(
        &self,
        delta: vixen_net::CookieJarDelta,
    ) -> Result<(), EngineError> {
        self.runtime_network_state
            .apply_cookie_delta(delta)
            .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))
    }

    /// Execute parser classics followed by deferred modules in the persistent
    /// page realm for `page`.
    ///
    /// This is the page-script trust boundary: response-header CSP is active
    /// first, document meta CSP takes effect for later scripts as it is
    /// encountered, external scripts resolve against the document base URL,
    /// HTTP(S) fetches cross `vixen-net` URL policy, and `nosniff` is enforced
    /// before execution. Blocked/failed subresources are skipped; JavaScript
    /// exceptions still surface as [`codes::SCRIPT_EVAL`] errors.
    #[cfg(test)]
    pub fn execute_page_scripts(&mut self, page: &mut Page) -> Result<usize, EngineError> {
        self.execute_page_scripts_with_csp_bypass(page, false)
    }

    /// Execute parser classic/module scripts with an explicit inspector/CDP CSP override.
    ///
    /// The default product path must call [`Self::execute_page_scripts`] so CSP
    /// remains fail-closed. CDP `Page.setBypassCSP` uses this method for the
    /// DevTools/automation trust boundary where the inspector has explicitly
    /// opted into disabling script-src checks for subsequent navigations.
    #[cfg(test)]
    pub fn execute_page_scripts_with_csp_bypass(
        &mut self,
        page: &mut Page,
        bypass_csp: bool,
    ) -> Result<usize, EngineError> {
        let mut runner = PageScriptRunner::new(page, bypass_csp);
        let mut executed = 0;
        while let Some(item) = runner.prepare_next(page) {
            match item {
                PreparedPageScript::Skip => {}
                PreparedPageScript::Inline { source, request } => {
                    self.ensure_realm(Some(&*page))?;
                    self.execute_in_current_realm(&source, Some(&request))?;
                    self.apply_dom_mutations(page)?;
                    executed += 1;
                }
                PreparedPageScript::InlineModule { source, request } => {
                    self.evaluate_module_with_page_mut(&source, &request, page)?;
                    executed += 1;
                }
                PreparedPageScript::ImportMap { .. } => {}
                PreparedPageScript::External(request) => {
                    let module = request.is_module();
                    if let Some(source) = load_external_page_script(&self.network_config, &request)?
                    {
                        if module {
                            self.evaluate_module_with_page_mut(&source, &request, page)?;
                        } else {
                            self.evaluate_with_page_mut(&source, page)?;
                        }
                        executed += 1;
                    }
                }
            }
        }
        self.run_document_tasks_with_page_mut(page)?;
        Ok(executed)
    }

    #[cfg(test)]
    fn run_document_tasks_with_page_mut(&mut self, page: &mut Page) -> Result<usize, EngineError> {
        let ids = document_task_ids(self.evaluate_with_page_mut(
            "JSON.stringify(globalThis.__vixenReadyDocumentTaskIds ? globalThis.__vixenReadyDocumentTaskIds(64) : [])",
            page,
        )?)?;
        let mut executed = 0;
        for id in ids {
            if self.evaluate_with_page_mut(
                &format!(
                    "globalThis.__vixenRunDocumentTask ? globalThis.__vixenRunDocumentTask({id}) : false"
                ),
                page,
            )? == JsValue::Bool(true)
            {
                executed += 1;
            }
        }
        Ok(executed)
    }

    pub(crate) fn run_document_tasks_with_shared_page_mut(
        &mut self,
        page: &Rc<RefCell<Page>>,
    ) -> Result<usize, EngineError> {
        let ids = document_task_ids(self.evaluate_with_shared_page_mut(
            "JSON.stringify(globalThis.__vixenReadyDocumentTaskIds ? globalThis.__vixenReadyDocumentTaskIds(64) : [])",
            page,
        )?)?;
        let mut executed = 0;
        for id in ids {
            if self.evaluate_with_shared_page_mut(
                &format!(
                    "globalThis.__vixenRunDocumentTask ? globalThis.__vixenRunDocumentTask({id}) : false"
                ),
                page,
            )? == JsValue::Bool(true)
            {
                executed += 1;
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
        self.module_loader.reset_realm();
        self.runtime = None;
        self.dom_mutations = None;
        self.realm_key = RealmKey::NoPage;
        self.cancelled_module_realm = false;
    }

    pub(crate) fn recover_cancelled_module_realm(
        &mut self,
        page: &Rc<RefCell<Page>>,
    ) -> Result<(), EngineError> {
        if !self.cancelled_module_realm {
            return Ok(());
        }
        self.reset_realm();
        self.ensure_realm(Some(&page.borrow()))
    }

    /// Refresh page-backed host snapshots after parser-blocking resources have
    /// changed style/layout but before any author script executes.
    pub(crate) fn refresh_page_hosts(&mut self, page: &Page) -> Result<(), EngineError> {
        let runtime = self.runtime.as_mut().ok_or_else(|| {
            EngineError::script(codes::SCRIPT_EVAL, "page runtime is not initialised")
        })?;
        cssom::refresh(runtime, page)?;
        if let Some(mutations) = self.dom_mutations.clone() {
            dom::refresh(runtime, page, mutations)?;
        }
        self.realm_key = RealmKey::Page(page_realm_key(page));
        Ok(())
    }

    /// Drain console calls recorded in the current realm. CDP uses this after
    /// `Runtime.evaluate`, page-script execution, and synthetic input dispatch;
    /// callers that have not created a realm simply get an empty list.
    pub fn drain_console_events(&mut self) -> Result<Vec<JsConsoleEvent>, EngineError> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(Vec::new());
        };
        let result = runtime::execute_script_immediate(
            runtime,
            "vixen-console-drain.js",
            "JSON.stringify(globalThis.__vixenDrainConsoleEvents ? globalThis.__vixenDrainConsoleEvents() : [])".to_owned(),
            &self.runtime_interrupt,
        )?;
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
        let result = runtime::execute_script_immediate(
            runtime,
            "vixen-dialog-drain.js",
            "JSON.stringify(globalThis.__vixenDrainDialogEvents ? globalThis.__vixenDrainDialogEvents() : [])".to_owned(),
            &self.runtime_interrupt,
        )?;
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
        let result = runtime::execute_script_immediate(
            runtime,
            "vixen-binding-drain.js",
            "JSON.stringify(globalThis.__vixenDrainBindingEvents ? globalThis.__vixenDrainBindingEvents() : [])".to_owned(),
            &self.runtime_interrupt,
        )?;
        match runtime::js_value_from_global(runtime, result)? {
            JsValue::String(json) => parse_binding_events(&json),
            _ => Ok(Vec::new()),
        }
    }

    /// Drain fetch() network lifecycle events recorded in the current realm.
    pub fn drain_network_events(&mut self) -> Result<Vec<JsNetworkEvent>, EngineError> {
        let mut events = self
            .module_loader
            .drain_events()
            .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?;
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(events);
        };
        let result = runtime::execute_script_immediate(
            runtime,
            "vixen-network-drain.js",
            "JSON.stringify(globalThis.__vixenDrainNetworkEvents ? globalThis.__vixenDrainNetworkEvents() : [])".to_owned(),
            &self.runtime_interrupt,
        )?;
        match runtime::js_value_from_global(runtime, result)? {
            JsValue::String(json) => {
                events.extend(parse_network_events(&json)?);
                Ok(events)
            }
            _ => Ok(events),
        }
    }

    /// Drain navigation/history/form-submit actions recorded in the current
    /// page realm. Non-page realms and pages without queued actions return an
    /// empty list.
    pub fn drain_navigation_actions(&mut self) -> Result<Vec<JsNavigationAction>, EngineError> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(Vec::new());
        };
        let result = runtime::execute_script_immediate(
            runtime,
            "vixen-navigation-drain.js",
            "JSON.stringify(globalThis.__vixenDrainNavigationActions ? globalThis.__vixenDrainNavigationActions() : [])".to_owned(),
            &self.runtime_interrupt,
        )?;
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
        let request = page.and_then(|page| {
            ExternalPageScript::automation(page, false, self.module_loader.document_import_map())
        });
        self.execute_in_current_realm(src, request.as_ref())
    }

    fn execute_in_current_realm(
        &mut self,
        src: &str,
        request: Option<&ExternalPageScript>,
    ) -> Result<JsValue, EngineError> {
        if let Some(request) = request {
            self.module_loader
                .begin_graph(request.dynamic_import_root())
                .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?;
        }
        let source_name = request
            .map(|request| request.url().to_string())
            .unwrap_or_else(|| "inline.js".to_owned());
        let result = {
            let runtime = self.runtime.as_mut().expect("realm initialised");
            runtime::execute_script(
                runtime,
                source_name,
                src.to_owned(),
                &self.runtime_interrupt,
                self.event_loop_executor
                    .as_ref()
                    .expect("event-loop executor is alive"),
            )
            .and_then(|value| runtime::js_value_from_global(runtime, value))
        };
        if result.as_ref().is_err_and(|error| {
            matches!(
                error.code(),
                codes::SCRIPT_INTERRUPTED | codes::SCRIPT_TIMEOUT
            ) && self.module_loader.has_pending_loads()
        }) {
            self.module_loader.cancel_pending_loads();
            self.cancelled_module_realm = true;
        }
        result
    }

    fn execute_module_in_current_realm(
        &mut self,
        source: &str,
        request: &ExternalPageScript,
    ) -> Result<(), EngineError> {
        self.module_loader
            .begin_graph(request.clone())
            .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?;
        let result = {
            let runtime = self.runtime.as_mut().expect("realm initialised");
            runtime::execute_module(
                runtime,
                request.url().clone(),
                source.to_owned(),
                &self.runtime_interrupt,
                self.event_loop_executor
                    .as_ref()
                    .expect("event-loop executor is alive"),
            )
        };
        if result.as_ref().is_err_and(|error| {
            matches!(
                error.code(),
                codes::SCRIPT_INTERRUPTED | codes::SCRIPT_TIMEOUT
            ) && self.module_loader.has_pending_loads()
        }) {
            self.module_loader.cancel_pending_loads();
            self.cancelled_module_realm = true;
        }
        result
    }

    fn ensure_realm(&mut self, page: Option<&Page>) -> Result<(), EngineError> {
        let target = page
            .map(page_realm_key)
            .map(RealmKey::Page)
            .unwrap_or(RealmKey::NoPage);
        if self.realm_key != target || self.runtime.is_none() {
            self.module_loader.reset_realm();
            self.runtime = None;
            self.storage_opaque_serial = self.storage_opaque_serial.saturating_add(1);
            let storage = web_storage_host(
                page,
                &self.storage_backend,
                &self.storage_session_id,
                self.storage_opaque_serial,
            );
            let init = {
                let _executor_guard = self
                    .event_loop_executor
                    .as_ref()
                    .expect("event-loop executor is alive")
                    .enter();
                runtime::new_deno_runtime(
                    page,
                    runtime::DenoRuntimeConfig {
                        network: self.network_config.clone(),
                        storage,
                        network_state: self.runtime_network_state.clone(),
                        extra_http_headers: self.extra_http_headers.clone(),
                        cache_disabled: self.cache_disabled.clone(),
                        permission_overrides: self.permission_overrides.clone(),
                        interrupt: self.runtime_interrupt.clone(),
                        synchronous_layout: self.synchronous_layout.clone(),
                        module_loader: self.module_loader.clone(),
                    },
                )?
            };
            self.runtime = Some(init.runtime);
            self.dom_mutations = init.dom_mutations;
            self.realm_key = target;
            if self.record_visits_on_realm
                && let Some(page) = page
            {
                self.record_page_visit(page)?;
            }
        }
        Ok(())
    }

    fn record_page_visit(&self, page: &Page) -> Result<(), EngineError> {
        let webapi::WebStorageBackend::Store(store) = &self.storage_backend else {
            return Ok(());
        };
        let ts = current_unix_timestamp();
        store
            .record_visit(&page_origin(page).partition_key(), page.url(), ts)
            .map_err(|err| EngineError::Other {
                code: codes::SCRIPT_EVAL,
                message: format!("history store write failed: {err}"),
            })
    }

    fn apply_dom_mutations(&mut self, page: &mut Page) -> Result<bool, EngineError> {
        let Some(sink) = self.dom_mutations.as_ref() else {
            return Ok(false);
        };
        let mutations = sink.take();
        if mutations.is_empty() {
            if let Some(runtime) = self.runtime.as_mut() {
                cssom::refresh(runtime, page)?;
            }
            return Ok(false);
        }
        apply_dom_mutation_list(page, mutations)?;
        if let Some(runtime) = self.runtime.as_mut() {
            cssom::refresh(runtime, page)?;
        }
        self.realm_key = RealmKey::Page(page_realm_key(page));
        Ok(true)
    }

    fn discard_dom_mutations(&self) {
        if let Some(sink) = self.dom_mutations.as_ref() {
            sink.take();
        }
    }
}

fn apply_dom_mutation_list(
    page: &mut Page,
    mutations: Vec<dom::DomMutation>,
) -> Result<(), EngineError> {
    for mutation in mutations {
        match mutation {
            dom::DomMutation::SetDocumentTitle { value } => page
                .set_title(&value)
                .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?,
            dom::DomMutation::SetTextContent { node_id, value } => {
                page.set_element_text_content(node_id, &value)
                    .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?;
            }
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
            dom::DomMutation::SetInnerHtml { node_id, html } => {
                page.set_element_inner_html(node_id, &html)
                    .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?;
            }
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
            dom::DomMutation::SetControlSelection {
                node_id,
                element_id,
                name,
                tag,
                base_offset,
                extent_offset,
            } => page
                .set_form_control_selection(
                    node_id,
                    element_id.as_deref(),
                    name.as_deref(),
                    &tag,
                    base_offset,
                    extent_offset,
                )
                .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?,
            dom::DomMutation::SetContenteditableState {
                node_id,
                value,
                base_offset,
                extent_offset,
            } => page
                .set_contenteditable_text_state(node_id, &value, base_offset, extent_offset)
                .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?,
            dom::DomMutation::SetFocusedElement { node_id } => {
                page.set_focused_element_node_id(node_id);
            }
            dom::DomMutation::SetSelection { selection } => {
                page.set_selection(selection);
            }
            dom::DomMutation::SetRootScroll { x, y } => {
                page.scroll_root_to((x, y));
            }
            dom::DomMutation::SetElementScroll {
                node_id,
                element_id,
                tag,
                x,
                y,
            } => {
                page.set_element_scroll(node_id, element_id.as_deref(), &tag, (x, y));
            }
        }
    }
    Ok(())
}

impl PageScriptRunner {
    pub(crate) fn new(page: &Page, bypass_csp: bool) -> Self {
        Self {
            items: page.document().script_execution_items().into_iter(),
            deferred_modules: std::collections::VecDeque::new(),
            csp: page.csp().clone(),
            origin: page_origin(page),
            bypass_csp,
            next_inline_classic: 0,
            next_inline_module: 0,
            import_map: None,
            import_map_seen: false,
            module_seen: false,
        }
    }

    /// Prepare one parser-discovered item without loading external resources.
    /// `None` means the sequence is complete.
    pub(crate) fn prepare_next(&mut self, page: &Page) -> Option<PreparedPageScript> {
        loop {
            let Some(item) = self.items.next() else {
                return self.deferred_modules.pop_front();
            };
            let prepared = match item {
                DocumentScriptItem::CspMeta(policy) => {
                    if !self.bypass_csp {
                        self.csp.add_header(&policy);
                    }
                    return Some(PreparedPageScript::Skip);
                }
                DocumentScriptItem::InlineClassicScript(script) => {
                    if self.inline_allowed(&script) {
                        self.next_inline_classic = self.next_inline_classic.saturating_add(1);
                        let mut url = url::Url::parse(&page.document_base_uri())
                            .or_else(|_| url::Url::parse(page.url()))
                            .unwrap_or_else(|_| {
                                url::Url::parse("about:blank")
                                    .expect("static classic-script base URL")
                            });
                        url.set_fragment(Some(&format!(
                            "vixen-inline-classic-{}",
                            self.next_inline_classic
                        )));
                        return Some(PreparedPageScript::Inline {
                            source: script.source,
                            request: Box::new(ExternalPageScript {
                                url,
                                csp: (!self.bypass_csp).then(|| self.csp.clone()),
                                origin: self.origin.clone(),
                                nonce: script.nonce,
                                context_trustworthy: url::Url::parse(page.url())
                                    .ok()
                                    .as_ref()
                                    .is_some_and(
                                        vixen_net::referrer_policy::is_potentially_trustworthy,
                                    ),
                                module: false,
                                module_credentials: ModuleCredentialsMode::SameOrigin,
                                import_map: self.import_map.clone(),
                                integrity: None,
                            }),
                        });
                    }
                    return Some(PreparedPageScript::Skip);
                }
                DocumentScriptItem::ExternalClassicScript(script) => {
                    return Some(self.prepare_external(page, script, false));
                }
                DocumentScriptItem::InlineModuleScript(script) => {
                    self.module_seen = true;
                    if !self.inline_allowed(&script) {
                        continue;
                    }
                    self.next_inline_module = self.next_inline_module.saturating_add(1);
                    let mut specifier =
                        url::Url::parse(&page.document_base_uri()).unwrap_or_else(|_| {
                            url::Url::parse("about:blank").expect("static module base URL")
                        });
                    specifier.set_fragment(Some(&format!(
                        "vixen-inline-module-{}",
                        self.next_inline_module
                    )));
                    PreparedPageScript::InlineModule {
                        source: script.source,
                        request: Box::new(ExternalPageScript {
                            url: specifier,
                            csp: (!self.bypass_csp).then(|| self.csp.clone()),
                            origin: self.origin.clone(),
                            nonce: None,
                            context_trustworthy: url::Url::parse(page.url())
                                .ok()
                                .as_ref()
                                .is_some_and(
                                    vixen_net::referrer_policy::is_potentially_trustworthy,
                                ),
                            module: true,
                            module_credentials: ModuleCredentialsMode::SameOrigin,
                            import_map: self.import_map.clone(),
                            integrity: None,
                        }),
                    }
                }
                DocumentScriptItem::ExternalModuleScript(script) => {
                    self.module_seen = true;
                    self.prepare_external(page, script, true)
                }
                DocumentScriptItem::ImportMap(import_map) => {
                    return Some(self.prepare_import_map(page, import_map));
                }
            };
            if !matches!(prepared, PreparedPageScript::Skip) {
                self.deferred_modules.push_back(prepared);
            }
        }
    }

    fn inline_allowed(&self, script: &crate::doc::InlineScript) -> bool {
        self.bypass_csp
            || self.csp.allows_inline_script(
                &self.origin,
                Some(&script.source),
                script.nonce.as_deref(),
            )
    }

    fn prepare_external(
        &self,
        page: &Page,
        script: crate::doc::ExternalScript,
        module: bool,
    ) -> PreparedPageScript {
        let Some(url) = resolve_external_script_url(page, &script.src) else {
            return PreparedPageScript::Skip;
        };
        let uses_cors = module
            || script
                .integrity
                .as_deref()
                .is_some_and(|value| !value.is_empty());
        let request = ExternalPageScript {
            url,
            csp: (!self.bypass_csp).then(|| self.csp.clone()),
            origin: self.origin.clone(),
            nonce: script.nonce,
            context_trustworthy: url::Url::parse(page.url())
                .ok()
                .as_ref()
                .is_some_and(vixen_net::referrer_policy::is_potentially_trustworthy),
            module,
            module_credentials: if uses_cors
                && script
                    .cross_origin
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case("use-credentials"))
            {
                ModuleCredentialsMode::Include
            } else {
                ModuleCredentialsMode::SameOrigin
            },
            import_map: self.import_map.clone(),
            integrity: script.integrity,
        };
        if request.allows_url(request.url()) {
            PreparedPageScript::External(Box::new(request))
        } else {
            PreparedPageScript::Skip
        }
    }

    fn prepare_import_map(
        &mut self,
        page: &Page,
        import_map: crate::doc::InlineImportMap,
    ) -> PreparedPageScript {
        if self.import_map_seen {
            return PreparedPageScript::ImportMap {
                diagnostics: Vec::new(),
                error: Some("multiple import maps are not supported".to_owned()),
            };
        }
        self.import_map_seen = true;
        if self.module_seen {
            return PreparedPageScript::ImportMap {
                diagnostics: Vec::new(),
                error: Some("import maps after module discovery are not supported".to_owned()),
            };
        }
        if import_map.src.is_some() {
            return PreparedPageScript::ImportMap {
                diagnostics: Vec::new(),
                error: Some("external import maps are not supported".to_owned()),
            };
        }
        if !self.inline_source_allowed(&import_map.source, import_map.nonce.as_deref()) {
            return PreparedPageScript::Skip;
        }
        let base_url = match url::Url::parse(&page.document_base_uri()) {
            Ok(base_url) => base_url,
            Err(error) => {
                return PreparedPageScript::ImportMap {
                    diagnostics: Vec::new(),
                    error: Some(format!("import map base URL is invalid: {error}")),
                };
            }
        };
        match import_maps::parse_inline_import_map(&import_map.source, base_url) {
            Ok(parsed) => {
                self.import_map = Some(parsed.map);
                PreparedPageScript::ImportMap {
                    diagnostics: parsed.diagnostics,
                    error: None,
                }
            }
            Err(error) => PreparedPageScript::ImportMap {
                diagnostics: Vec::new(),
                error: Some(error),
            },
        }
    }

    fn inline_source_allowed(&self, source: &str, nonce: Option<&str>) -> bool {
        self.bypass_csp
            || self
                .csp
                .allows_inline_script(&self.origin, Some(source), nonce)
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

fn document_task_ids(value: JsValue) -> Result<Vec<u64>, EngineError> {
    let JsValue::String(json) = value else {
        return Err(EngineError::script(
            codes::SCRIPT_EVAL,
            "document task queue did not return JSON",
        ));
    };
    let ids = deno_core::serde_json::from_str::<Vec<u64>>(&json).map_err(|error| {
        EngineError::script(
            codes::SCRIPT_EVAL,
            format!("document task queue parse failed: {error}"),
        )
    })?;
    if ids.len() > 64 {
        return Err(EngineError::script(
            codes::SCRIPT_EVAL,
            "document task queue exceeded its bounded batch",
        ));
    }
    Ok(ids)
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

fn parse_network_events(json: &str) -> Result<Vec<JsNetworkEvent>, EngineError> {
    let value: deno_core::serde_json::Value =
        deno_core::serde_json::from_str(json).map_err(|err| {
            EngineError::script(
                codes::SCRIPT_EVAL,
                format!("network event parse failed: {err}"),
            )
        })?;
    let Some(events) = value.as_array() else {
        return Ok(Vec::new());
    };
    events.iter().map(parse_network_event).collect()
}

fn parse_network_event(
    value: &deno_core::serde_json::Value,
) -> Result<JsNetworkEvent, EngineError> {
    let request_id = required_network_event_string(value, "requestId")?;
    match value
        .get("type")
        .and_then(deno_core::serde_json::Value::as_str)
        .unwrap_or_default()
    {
        "request" => Ok(JsNetworkEvent::Request {
            request_id,
            url: required_network_event_string(value, "url")?,
            method: value
                .get("method")
                .and_then(deno_core::serde_json::Value::as_str)
                .unwrap_or("GET")
                .to_ascii_uppercase(),
        }),
        "redirect" => Ok(JsNetworkEvent::Redirect {
            request_id,
            from: required_network_event_string(value, "from")?,
            to: required_network_event_string(value, "to")?,
            status: value
                .get("status")
                .and_then(deno_core::serde_json::Value::as_u64)
                .unwrap_or_default()
                .min(u16::MAX as u64) as u16,
        }),
        "response" => Ok(JsNetworkEvent::Response {
            request_id,
            url: required_network_event_string(value, "url")?,
            status: value
                .get("status")
                .and_then(deno_core::serde_json::Value::as_u64)
                .unwrap_or_default()
                .min(u16::MAX as u64) as u16,
        }),
        "progress" => Ok(JsNetworkEvent::Progress {
            request_id,
            url: required_network_event_string(value, "url")?,
            chunk_bytes: network_event_u64(value, "chunkBytes"),
            loaded_bytes: network_event_u64(value, "loadedBytes"),
            total_bytes: value
                .get("totalBytes")
                .and_then(deno_core::serde_json::Value::as_u64),
        }),
        "completed" => Ok(JsNetworkEvent::Completed {
            request_id,
            url: required_network_event_string(value, "url")?,
            body_bytes: network_event_u64(value, "bodyBytes"),
        }),
        "failure" => Ok(JsNetworkEvent::Failure {
            request_id,
            url: required_network_event_string(value, "url")?,
            error_text: value
                .get("errorText")
                .and_then(deno_core::serde_json::Value::as_str)
                .unwrap_or("fetch failed")
                .to_owned(),
            blocked_reason: value
                .get("blockedReason")
                .and_then(deno_core::serde_json::Value::as_str)
                .filter(|reason| !reason.is_empty())
                .map(ToOwned::to_owned),
        }),
        other => Err(EngineError::script(
            codes::SCRIPT_EVAL,
            format!("unsupported network event: {other}"),
        )),
    }
}

fn network_event_u64(value: &deno_core::serde_json::Value, name: &str) -> u64 {
    value
        .get(name)
        .and_then(deno_core::serde_json::Value::as_u64)
        .unwrap_or_default()
}

fn required_network_event_string(
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
                format!("network event missing string field `{name}`"),
            )
        })
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
            submitter_node_id: value
                .get("submitterNodeId")
                .and_then(deno_core::serde_json::Value::as_u64)
                .filter(|node_id| *node_id != 0)
                .map(|node_id| node_id as usize),
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
            enctype: value
                .get("enctype")
                .and_then(deno_core::serde_json::Value::as_str)
                .unwrap_or("application/x-www-form-urlencoded")
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
        "history-scroll-restoration" => Ok(JsNavigationAction::HistoryScrollRestoration {
            value: required_action_string(value, "value")?,
        }),
        "overflow" => Ok(JsNavigationAction::Overflow),
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

pub(crate) fn merge_profile_cookies(
    store: &vixen_store::Store,
    url: &url::Url,
    jar: &mut vixen_net::CookieJar,
    profile_baseline: &mut Vec<vixen_net::CookieSnapshot>,
) -> Result<(), EngineError> {
    webapi::merge_profile_cookies(store, url, jar, profile_baseline)
        .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))
}

pub(crate) fn element_scroll_state_source(page: &Page, emit_scroll: bool) -> String {
    dom::element_scroll_state_source(page, emit_scroll)
}

pub(crate) fn persist_profile_cookies(
    store: &vixen_store::Store,
    urls: &[url::Url],
    delta: &vixen_net::CookieJarDelta,
) -> Result<(), EngineError> {
    for url in urls {
        webapi::persist_profile_cookie_delta(store, url, delta)
            .map_err(|message| EngineError::script(codes::SCRIPT_EVAL, message))?;
    }
    Ok(())
}

fn current_unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
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
        let partition = StoragePartition::new(origin, kind).partition_key();
        return match kind {
            StorageKind::Local => partition,
            StorageKind::Session => {
                format!("{partition}:context:{}", stable_storage_hash(session_id))
            }
        };
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

#[cfg(test)]
fn load_external_page_script(
    network_config: &vixen_net::NetworkConfig,
    request: &ExternalPageScript,
) -> Result<Option<String>, EngineError> {
    match request.url().scheme() {
        "file" => Ok(load_file_script(
            request.url(),
            network_config.max_body_bytes,
        )),
        "http" | "https" => {
            let response = match fetch_http_script(network_config.clone(), request.url().clone()) {
                Ok(response) => response,
                Err(_) => return Ok(None),
            };
            let final_url = match url::Url::parse(&response.final_url) {
                Ok(url) => url,
                Err(_) => return Ok(None),
            };
            if request.allows_url(&final_url) && script_response_allowed(&response) {
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

#[cfg(test)]
fn load_file_script(url: &url::Url, max_body_bytes: u64) -> Option<String> {
    let path = url.to_file_path().ok()?;
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > max_body_bytes {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut bytes =
        Vec::with_capacity(usize::try_from(metadata.len().min(max_body_bytes)).unwrap_or_default());
    file.take(max_body_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if (bytes.len() as u64) > max_body_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn script_response_allowed(response: &vixen_net::TextResponse) -> bool {
    if !(200..300).contains(&response.status) {
        return false;
    }
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
        self.module_loader.shutdown();
        self.storage_backend = webapi::WebStorageBackend::memory();
        if let Some(executor) = self.event_loop_executor.take() {
            executor.shutdown_background();
        }
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

    fn spawn_stalled_fetch_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let disconnected = std::sync::Arc::new(AtomicBool::new(false));
        let server_disconnected = disconnected.clone();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\nx",
                )
                .unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut byte = [0_u8; 1];
            if matches!(stream.read(&mut byte), Ok(0)) {
                server_disconnected.store(true, Ordering::SeqCst);
            }
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/payload", addr.port()),
            config,
            disconnected,
            handle,
        )
    }

    type GatedFetchServer = (
        String,
        vixen_net::NetworkConfig,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
        std::thread::JoinHandle<()>,
    );

    fn spawn_gated_streaming_fetch_server(host: &str) -> GatedFetchServer {
        use std::io::{Read, Write};
        use std::sync::mpsc;
        use std::time::Duration;

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (headers_tx, headers_rx) = mpsc::channel();
        let (first_release_tx, first_release_rx) = mpsc::channel();
        let (first_sent_tx, first_sent_rx) = mpsc::channel();
        let (finish_release_tx, finish_release_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Vixen-Stream: yes\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            stream.flush().unwrap();
            headers_tx.send(()).unwrap();
            first_release_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("response should resolve before the first body chunk");
            stream.write_all(b"6\r\nfirst-\r\n").unwrap();
            stream.flush().unwrap();
            first_sent_tx.send(()).unwrap();
            finish_release_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("first stream read should resolve before completion");
            stream.write_all(b"6\r\nsecond\r\n0\r\n\r\n").unwrap();
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/payload", addr.port()),
            config,
            headers_rx,
            first_release_tx,
            first_sent_rx,
            finish_release_tx,
            handle,
        )
    }

    fn spawn_delayed_fetch_server(
        host: &str,
        body: &str,
        delay: std::time::Duration,
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
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes()).unwrap();
            stream.flush().unwrap();
            std::thread::sleep(delay);
            let _ = stream.write_all(body.as_bytes());
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/payload", addr.port()),
            config,
            handle,
        )
    }

    fn spawn_header_echo_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            let body = if request.contains("\r\nx-vixen-test: yes\r\n")
                && !request.contains("\r\nhost: evil.example\r\n")
            {
                "header-ok"
            } else {
                "header-missing"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

    fn spawn_body_echo_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let header_end = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|pos| pos + 4)
                        .unwrap_or(request.len());
                    let headers =
                        String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                    let content_length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or_default();
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
            }
            let header_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|pos| pos + 4)
                .unwrap_or(request.len());
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let method = headers.split_whitespace().next().unwrap_or_default();
            let content_type = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-type:"))
                .map(str::trim)
                .unwrap_or("missing");
            let body = String::from_utf8_lossy(&request[header_end..]);
            let body = format!("{method}:{content_type}:{body}");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

    fn spawn_preflight_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let header_end = request
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .map(|pos| pos + 4)
                            .unwrap_or(request.len());
                        let headers =
                            String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                        let content_length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or_default();
                        if request.len() >= header_end + content_length {
                            break;
                        }
                    }
                }
                let header_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|pos| pos + 4)
                    .unwrap_or(request.len());
                let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                if index == 0 {
                    let ok = headers.starts_with("options ")
                        && headers.contains("\r\norigin: http://source.test\r\n")
                        && headers.contains("\r\naccess-control-request-method: post\r\n")
                        && headers
                            .contains("\r\naccess-control-request-headers: x-vixen-custom\r\n");
                    let status = if ok {
                        "204 No Content"
                    } else {
                        "400 Bad Request"
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nAccess-Control-Allow-Origin: http://source.test\r\nAccess-Control-Allow-Methods: POST\r\nAccess-Control-Allow-Headers: X-Vixen-Custom\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                } else {
                    let body_text = String::from_utf8_lossy(&request[header_end..]);
                    let ok = headers.starts_with("post ")
                        && headers.contains("\r\nx-vixen-custom: yes\r\n")
                        && body_text == "preflight body";
                    let body = if ok {
                        "preflight-ok"
                    } else {
                        "preflight-missing"
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: http://source.test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            }
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/payload", addr.port()),
            config,
            handle,
        )
    }

    fn spawn_cached_preflight_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let preflights = std::sync::Arc::new(AtomicUsize::new(0));
        let server_preflights = preflights.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                if request.starts_with("options ") {
                    server_preflights.fetch_add(1, Ordering::SeqCst);
                    stream
                        .write_all(
                            b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: http://source.test\r\nAccess-Control-Allow-Methods: POST\r\nAccess-Control-Allow-Headers: X-Vixen-Custom\r\nAccess-Control-Max-Age: 600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                } else {
                    let body = "cached-preflight-ok";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: http://source.test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            }
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/payload", addr.port()),
            config,
            preflights,
            handle,
        )
    }

    fn spawn_referrer_echo_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            let body = if request.contains("\r\nreferer: http://source.test/path?q=1\r\n") {
                "referrer-ok"
            } else {
                "referrer-missing"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: http://source.test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

    fn spawn_cors_server(
        host: &str,
        allow_origin: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let allow_origin = allow_origin.to_owned();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let read = stream.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            let body = if request.contains("\r\norigin: http://source.test\r\n") {
                "cors-ok"
            } else {
                "cors-origin-missing"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Vixen-Test: yes\r\nX-Hidden: secret\r\nAccess-Control-Allow-Origin: {allow_origin}\r\nAccess-Control-Expose-Headers: X-Vixen-Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

    fn spawn_revalidation_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                if index == 0 {
                    let body = "cached-v1";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nETag: \"v1\"\r\nLast-Modified: Wed, 21 Oct 2015 07:28:00 GMT\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                } else if request.contains("\r\nif-none-match: \"v1\"\r\n")
                    && request.contains("\r\nif-modified-since: wed, 21 oct 2015 07:28:00 gmt\r\n")
                {
                    stream
                        .write_all(
                            b"HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                } else {
                    let body = "missing-conditional";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            }
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/payload", addr.port()),
            config,
            handle,
        )
    }

    fn spawn_expires_revalidation_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, SystemTime};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let date = httpdate::fmt_http_date(SystemTime::now());
        let expires = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(600));
        let handle = std::thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                server_requests.fetch_add(1, Ordering::SeqCst);
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).unwrap_or_default();
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                if index == 0 {
                    let body = "expires-v1";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nDate: {date}\r\nExpires: {expires}\r\nETag: \"expires-v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                } else if request.contains("\r\ncache-control: no-cache\r\n")
                    && request.contains("\r\nif-none-match: \"expires-v1\"\r\n")
                {
                    stream
                        .write_all(
                            b"HTTP/1.1 304 Not Modified\r\nETag: \"expires-v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                } else {
                    stream
                        .write_all(
                            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                }
            }
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/payload", addr.port()),
            config,
            requests,
            handle,
        )
    }

    fn spawn_fresh_vary_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline && server_requests.load(Ordering::SeqCst) < 2 {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("fresh cache server accept failed: {error}"),
                };
                let index = server_requests.fetch_add(1, Ordering::SeqCst) + 1;
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).unwrap_or_default();
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                let language = request
                    .lines()
                    .find_map(|line| line.strip_prefix("accept-language:"))
                    .map(str::trim)
                    .unwrap_or("missing");
                let body = format!("response-{index}-{language}");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: private, max-age=600\r\nVary: Accept-Language\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/payload", addr.port()),
            config,
            requests,
            handle,
        )
    }

    fn spawn_redirect_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let body = "redirect body";
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: /target\r\nContent-Type: text/plain\r\nX-Vixen-Redirect: yes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/redirect", addr.port()),
            config,
            handle,
        )
    }

    fn spawn_cross_host_redirect_server(
        initial_host: &str,
        target_host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let target_url = format!("http://{target_host}:{}/target", addr.port());
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = Arc::clone(&requests);
        let handle = std::thread::spawn(move || {
            let (mut initial, _) = listener.accept().unwrap();
            server_requests.fetch_add(1, Ordering::SeqCst);
            let mut request = [0_u8; 2048];
            let _ = initial.read(&mut request);
            initial
                .write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {target_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .unwrap();

            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_millis(300);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut target, _)) => {
                        server_requests.fetch_add(1, Ordering::SeqCst);
                        let _ = target.read(&mut request);
                        let _ = target.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ntarget",
                        );
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("redirect target accept failed: {error}"),
                }
            }
        });

        let mut config = vixen_net::NetworkConfig::default();
        config
            .dns_overrides
            .push((initial_host.to_owned(), vec![addr]));
        config
            .dns_overrides
            .push((target_host.to_owned(), vec![addr]));
        (
            format!("http://{initial_host}:{}/redirect", addr.port()),
            config,
            requests,
            handle,
        )
    }

    fn spawn_cacheable_redirect_server(
        host: &str,
    ) -> (
        String,
        String,
        vixen_net::NetworkConfig,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                server_requests.fetch_add(1, Ordering::SeqCst);
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).unwrap_or_default();
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                if path == "/redirect" {
                    stream
                        .write_all(
                            b"HTTP/1.1 301 Moved Permanently\r\nLocation: /target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                } else {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: max-age=600\r\nContent-Length: 12\r\nConnection: close\r\n\r\nalias-target",
                        )
                        .unwrap();
                }
            }
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (
            format!("http://{host}:{}/redirect", addr.port()),
            format!("http://{host}:{}/target", addr.port()),
            config,
            requests,
            handle,
        )
    }

    fn spawn_cookie_echo_server(
        host: &str,
    ) -> (
        String,
        vixen_net::NetworkConfig,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = first.read(&mut request);
            let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nSet-Cookie: sid=abc; Path=/\r\nContent-Length: 3\r\nConnection: close\r\n\r\nset";
            first.write_all(response.as_bytes()).unwrap();

            let (mut second, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let read = second.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]);
            let cookie = request
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("cookie").then(|| value.trim())
                })
                .unwrap_or("");
            let body = cookie.to_owned();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            second.write_all(response.as_bytes()).unwrap();
        });

        let mut config = vixen_net::NetworkConfig::default();
        config.dns_overrides.push((host.to_owned(), vec![addr]));
        (format!("http://{host}:{}", addr.port()), config, handle)
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
    fn runtime_jobs_timeout_and_leave_the_isolate_reusable() {
        let mut runtime = JsRuntime::new().expect("engine init");

        let loop_error = runtime
            .evaluate("for (;;) {}")
            .expect_err("infinite JavaScript must be interrupted");
        assert_eq!(loop_error.code(), codes::SCRIPT_TIMEOUT);
        assert_eq!(runtime.evaluate("20 + 22").unwrap(), JsValue::Int32(42));

        let promise_error = runtime
            .evaluate("new Promise(() => {})")
            .expect_err("an unresolved promise must be bounded");
        assert_eq!(promise_error.code(), codes::SCRIPT_EVAL);
        assert_eq!(runtime.evaluate("6 * 7").unwrap(), JsValue::Int32(42));
    }

    #[test]
    fn runtime_interrupt_is_immediate_and_leaves_the_isolate_reusable() {
        let mut runtime = JsRuntime::new().expect("engine init");
        let interrupt = runtime.interrupt_handle();
        let interrupter = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(25));
            assert!(interrupt.interrupt());
        });
        let started = std::time::Instant::now();

        let error = runtime
            .evaluate("for (;;) {}")
            .expect_err("browser lifecycle interruption must stop JavaScript");
        interrupter.join().unwrap();

        assert_eq!(error.code(), codes::SCRIPT_INTERRUPTED);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(150),
            "external interruption waited for the runtime timeout"
        );
        assert_eq!(runtime.evaluate("20 + 22").unwrap(), JsValue::Int32(42));
    }

    #[test]
    fn failed_page_evaluation_discards_deferred_dom_mutations() {
        let mut runtime = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "https://example.test/",
            "<!doctype html><title>before</title><body></body>",
        )
        .unwrap();

        let error = runtime
            .evaluate_with_page_mut("document.title = 'stale'; for (;;) {}", &mut page)
            .expect_err("infinite page script must be interrupted");
        assert_eq!(error.code(), codes::SCRIPT_TIMEOUT);
        assert_eq!(page.snapshot((800, 600)).title.as_deref(), Some("before"));

        assert_eq!(
            runtime
                .evaluate_with_page_mut("20 + 22", &mut page)
                .unwrap(),
            JsValue::Int32(42)
        );
        assert_eq!(page.snapshot((800, 600)).title.as_deref(), Some("before"));
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

        let form_page = Page::from_html(
            "file:///dom-formdata-host.html",
            "<form id='contact'><label id='name-label' for='name-input'>Name</label><input id='name-input' name='name' value='Ada'><label id='body-label'>Body<textarea name='body'>Hello</textarea></label><input type='checkbox' name='format' value='html' checked><select name='plan'><option value='free'>Free</option><option value='pro' selected>Pro</option></select><button id='reset-contact' type='reset'>Reset</button></form><form id='upload' enctype='multipart/form-data'><input type='file' name='attachment'></form>",
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
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const form = document.querySelector('#contact'); const input = form.querySelector('[name=name]'); const textarea = form.querySelector('[name=body]'); const checkbox = form.querySelector('[name=format]'); const select = form.querySelector('[name=plan]'); const events = []; form.addEventListener('reset', () => events.push('reset'), { once: true }); input.value = 'Grace'; textarea.value = 'Changed'; checkbox.checked = false; select.value = 'free'; form.reset(); return events.join(',') + ':' + input.value + ':' + textarea.value + ':' + checkbox.checked + ':' + select.value + ':' + select.selectedIndex; })()",
                &form_page
            )
            .unwrap(),
            JsValue::String("reset:Ada:Hello:true:pro:1".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "(() => { const form = document.querySelector('#contact'); const input = form.querySelector('[name=name]'); form.addEventListener('reset', (event) => event.preventDefault(), { once: true }); input.value = 'Grace'; form.querySelector('#reset-contact').click(); const canceled = input.value; form.reset(); return canceled + ':' + input.value; })()",
                &form_page
            )
            .unwrap(),
            JsValue::String("Grace:Ada".to_owned())
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
    fn web_storage_partitions_profile_local_and_context_session_state() {
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
            let same_context = Page::from_html("https://store.test/two", "<p>two</p>").unwrap();
            assert_eq!(
                rt.evaluate_with_page(
                    "localStorage.getItem('persist') + ':' + sessionStorage.getItem('tab')",
                    &same_context,
                )
                .unwrap(),
                JsValue::String("yes:one".to_owned())
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
                JsValue::String("yes:null".to_owned())
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
    fn permissions_query_reads_profile_store_and_defaults_prompt() {
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-permissions-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let url = url::Url::parse("https://permissions.test/app").unwrap();
        let origin_key = vixen_net::Origin::from_url(&url).partition_key();
        let store = vixen_store::Store::open(&path).unwrap();
        store
            .put_permission(&vixen_store::PermissionRecord {
                origin_key: origin_key.clone(),
                kind: "geolocation".to_owned(),
                decision: vixen_store::PermissionDecision::Granted,
                updated_unix: 1_000,
            })
            .unwrap();
        store
            .put_permission(&vixen_store::PermissionRecord {
                origin_key: origin_key.clone(),
                kind: "notifications".to_owned(),
                decision: vixen_store::PermissionDecision::Denied,
                updated_unix: 1_001,
            })
            .unwrap();
        store
            .put_permission(&vixen_store::PermissionRecord {
                origin_key,
                kind: "persistent-storage".to_owned(),
                decision: vixen_store::PermissionDecision::Granted,
                updated_unix: 1_002,
            })
            .unwrap();
        drop(store);

        let page = Page::from_html(url.as_str(), "<p>permissions</p>").unwrap();
        let mut rt = JsRuntime::with_storage_path(&path).expect("engine init");

        assert_eq!(
            rt.evaluate_with_page(
                "navigator.permissions.query({ name: 'geolocation' }).then((status) => status.state + ':' + (status instanceof PermissionStatus) + ':' + (navigator.permissions instanceof Permissions))",
                &page,
            )
            .unwrap(),
            JsValue::String("granted:true:true".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "navigator.permissions.query({ name: 'camera' }).then((status) => status.state)",
                &page,
            )
            .unwrap(),
            JsValue::String("prompt".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "navigator.permissions.query({ name: 'midi' }).then(() => false, (err) => err instanceof TypeError && /unsupported permission name/.test(err.message))",
                &page,
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            rt.evaluate_with_page("Notification.permission", &page)
                .unwrap(),
            JsValue::String("denied".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "Notification.requestPermission().then((permission) => permission)",
                &page
            )
            .unwrap(),
            JsValue::String("denied".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page("navigator.storage.persisted()", &page)
                .unwrap(),
            JsValue::Bool(true)
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn permission_overrides_replace_scope_and_reset_without_profile_writes() {
        let page = Page::from_html("https://permissions.test/app", "<p>permissions</p>").unwrap();
        let other = Page::from_html("https://other.test/app", "<p>other</p>").unwrap();
        let mut rt = JsRuntime::new().expect("engine init");

        rt.replace_permission_grants(
            Some("https://permissions.test".to_owned()),
            vec!["notifications".to_owned()],
        );
        assert_eq!(
            rt.evaluate_with_page(
                "Promise.all(['notifications','geolocation'].map((name) => navigator.permissions.query({ name }).then((status) => status.state))).then((states) => states.join(':'))",
                &page,
            )
            .unwrap(),
            JsValue::String("granted:denied".to_owned())
        );
        assert_eq!(
            rt.evaluate_with_page(
                "navigator.permissions.query({ name: 'notifications' }).then((status) => status.state)",
                &other,
            )
            .unwrap(),
            JsValue::String("prompt".to_owned())
        );

        rt.replace_permission_grants(None, vec!["geolocation".to_owned()]);
        assert_eq!(
            rt.evaluate_with_page(
                "navigator.permissions.query({ name: 'geolocation' }).then((status) => status.state)",
                &other,
            )
            .unwrap(),
            JsValue::String("granted".to_owned())
        );
        rt.reset_permission_overrides();
        assert_eq!(
            rt.evaluate_with_page(
                "navigator.permissions.query({ name: 'geolocation' }).then((status) => status.state)",
                &other,
            )
            .unwrap(),
            JsValue::String("prompt".to_owned())
        );
    }

    #[test]
    fn document_cookie_round_trips_through_profile_store() {
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-document-cookie-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let page = Page::from_html("http://cookie-doc.test/page.html", "<p>cookies</p>").unwrap();

        {
            let mut rt = JsRuntime::with_storage_path(&path).expect("engine init");
            assert_eq!(
                rt.evaluate_with_page(
                    "document.cookie = 'theme=dark; Path=/'; document.cookie",
                    &page,
                )
                .unwrap(),
                JsValue::String("theme=dark".to_owned())
            );
            assert_eq!(
                rt.evaluate_with_page(
                    "(() => { try { document.cookie = 'secret=x; HttpOnly'; } catch (err) { return err instanceof TypeError && /HttpOnly/.test(err.message); } return false; })()",
                    &page,
                )
                .unwrap(),
                JsValue::Bool(true)
            );
        }

        {
            let mut rt = JsRuntime::with_storage_path(&path).expect("engine init");
            let same_origin =
                Page::from_html("http://cookie-doc.test/other.html", "<p>again</p>").unwrap();
            assert_eq!(
                rt.evaluate_with_page("document.cookie", &same_origin)
                    .unwrap(),
                JsValue::String("theme=dark".to_owned())
            );

            let other_origin =
                Page::from_html("http://other-cookie.test/", "<p>other</p>").unwrap();
            assert_eq!(
                rt.evaluate_with_page("document.cookie", &other_origin)
                    .unwrap(),
                JsValue::String(String::new())
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn page_realm_creation_records_history_visits_in_profile_store() {
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-history-store-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let page_one = Page::from_html("https://history.test/one", "<p>one</p>").unwrap();
        let page_two = Page::from_html("https://history.test/two", "<p>two</p>").unwrap();

        {
            let mut rt = JsRuntime::with_storage_path(&path).expect("engine init");
            assert_eq!(
                rt.evaluate_with_page("1", &page_one).unwrap(),
                JsValue::Int32(1)
            );
            assert_eq!(
                rt.evaluate_with_page("2", &page_one).unwrap(),
                JsValue::Int32(2)
            );
            assert_eq!(
                rt.evaluate_with_page("3", &page_two).unwrap(),
                JsValue::Int32(3)
            );
        }

        let store = vixen_store::Store::open(&path).unwrap();
        let origin_key = page_origin(&page_one).partition_key();
        assert_eq!(
            store.visits_for(&origin_key, page_one.url()).unwrap().len(),
            1
        );
        assert_eq!(
            store.visits_for(&origin_key, page_two.url()).unwrap().len(),
            1
        );

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
    fn realm_teardown_discards_tasks_and_frame_globals_stay_fail_closed() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page_one = Page::from_html(
            "https://frames.test/one",
            "<iframe id='same' srcdoc='<p>same</p>'></iframe>\
             <iframe id='cross' src='https://other.test/frame'></iframe>",
        )
        .unwrap();
        let mut page_two =
            Page::from_html("https://frames.test/two", "<p>replacement</p>").unwrap();

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => {\
                   globalThis.__oldRealm = true;\
                   setTimeout(() => { globalThis.__staleTaskRan = true; }, 0);\
                   const same = document.querySelector('#same');\
                   const cross = document.querySelector('#cross');\
                   return [\
                     same.contentWindow === null, same.contentDocument === null,\
                     cross.contentWindow === null, cross.contentDocument === null\
                   ].join(':');\
                 })()",
                &mut page_one,
            )
            .unwrap(),
            JsValue::String("true:true:true:true".to_owned())
        );

        assert_eq!(
            rt.evaluate_with_page_mut(
                "typeof __oldRealm + ':' + typeof __staleTaskRan",
                &mut page_two,
            )
            .unwrap(),
            JsValue::String("undefined:undefined".to_owned())
        );
        assert_eq!(
            rt.run_document_tasks_with_page_mut(&mut page_two).unwrap(),
            0
        );
        assert_eq!(
            rt.evaluate_with_page("typeof __staleTaskRan", &page_two)
                .unwrap(),
            JsValue::String("undefined".to_owned())
        );
    }

    #[test]
    fn page_text_content_mutation_updates_page_and_renderer_source() {
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
        let snapshot = page
            .render_snapshot(
                vixen_api::BrowsingContextId::new(1).unwrap(),
                vixen_api::DocumentId::new(1).unwrap(),
                (200, 100),
                1,
                1.0,
                1.0,
            )
            .unwrap();
        assert!(snapshot.nodes.iter().any(|node| {
            matches!(&node.kind, vixen_api::RenderNodeKind::Text { text } if text == "clicked")
        }));
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
        assert!(page.text_content().contains("made"));
        assert!(page.text_content().contains("fresh"));
        assert!(page.text_content().contains("tail"));
    }

    #[test]
    fn live_dataset_reflects_attributes_and_advances_one_source_revision_per_write() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///dataset-mutate.html",
            "<html><head><style>\
               body { margin: 0; }\
               #target { display: block; width: 80px; height: 20px; }\
               #target[data-layout-mode='wide'] { width: 140px; height: 30px; }\
             </style></head><body>\
               <div id='target' data-role='copy' data-author-name='ada'>target</div>\
             </body></html>",
        )
        .unwrap();

        let initial_generation = page.renderer_source_generation();
        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 globalThis.__liveDataset = target.dataset;\
                 target.setAttribute('data-author-name', 'grace');\
                 return String(__liveDataset === target.dataset) + ':' +\
                   __liveDataset.authorName + ':' + target.dataset.authorName + ':' +\
                   Object.keys(__liveDataset).join(','); })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:grace:grace:role,authorName".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 __liveDataset.layoutMode = 'wide';\
                 return __liveDataset.layoutMode + ':' +\
                   target.getAttribute('data-layout-mode') + ':' +\
                   Object.keys(target.dataset).join(',') + ':' +\
                   String(__liveDataset === target.dataset); })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("wide:wide:role,authorName,layoutMode:true".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 2);
        let target_id = page.query_selector_all("#target").unwrap()[0].node_id;
        assert!(
            page.computed_style(target_id)
                .contains(&("width".to_owned(), "140px".to_owned()))
        );

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 return String(delete __liveDataset.role) + ':' +\
                   String(target.getAttribute('data-role')) + ':' +\
                   typeof target.dataset.role; })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:null:undefined".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 3);
    }

    #[test]
    fn live_class_list_reflects_attributes_and_advances_one_source_revision_per_write() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///class-list-mutate.html",
            "<html><head><style>\
               body { margin: 0; }\
               #target { display: block; width: 80px; height: 20px; }\
               #target.wide { width: 140px; }\
               #target.tall { height: 30px; }\
             </style></head><body>\
               <div id='target' class='compact'>target</div>\
             </body></html>",
        )
        .unwrap();

        let initial_generation = page.renderer_source_generation();
        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 globalThis.__liveClassList = target.classList;\
                 target.setAttribute('class', 'wide');\
                 return String(__liveClassList === target.classList) + ':' +\
                   __liveClassList.value + ':' + __liveClassList.contains('wide'); })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:wide:true".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);
        let target_id = page.query_selector_all("#target").unwrap()[0].node_id;
        assert!(
            page.computed_style(target_id)
                .contains(&("width".to_owned(), "140px".to_owned()))
        );

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 __liveClassList.add('tall');\
                 return target.getAttribute('class') + ':' +\
                   String(__liveClassList === target.classList) + ':' +\
                   Array.from(target.classList).join(','); })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("wide tall:true:wide,tall".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 2);
        assert!(
            page.computed_style(target_id)
                .contains(&("height".to_owned(), "30px".to_owned()))
        );
    }

    #[test]
    fn live_rel_list_reflects_attributes_and_advances_one_source_revision_per_write() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///rel-list-mutate.html",
            "<html><head><style>\
               #target { display: none; }\
               #target[rel~='wide'] { display: block; width: 140px; height: 20px; }\
               #target[rel~='tall'] { height: 30px; }\
             </style></head><body>\
               <a id='target' rel='compact'>target</a>\
             </body></html>",
        )
        .unwrap();

        let initial_generation = page.renderer_source_generation();
        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 globalThis.__liveRelList = target.relList;\
                 target.setAttribute('rel', 'wide');\
                 return String(__liveRelList === target.relList) + ':' +\
                   __liveRelList.value + ':' + __liveRelList.contains('wide'); })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:wide:true".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);
        let target_id = page.query_selector_all("#target").unwrap()[0].node_id;
        assert!(
            page.computed_style(target_id)
                .contains(&("width".to_owned(), "140px".to_owned()))
        );

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 __liveRelList.add('tall');\
                 return target.getAttribute('rel') + ':' +\
                   String(__liveRelList === target.relList) + ':' +\
                   Array.from(target.relList).join(','); })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("wide tall:true:wide,tall".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 2);
        assert!(
            page.computed_style(target_id)
                .contains(&("height".to_owned(), "30px".to_owned()))
        );
    }

    #[test]
    fn live_sandbox_list_reflects_attributes_and_advances_one_source_revision_per_write() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///sandbox-list-mutate.html",
            "<html><head><style>\
               #target { display: none; }\
               #target[sandbox~='allow-same-origin'] { display: block; width: 140px; height: 20px; }\
               #target[sandbox~='allow-forms'] { height: 30px; }\
             </style></head><body>\
               <iframe id='target' sandbox='allow-scripts'></iframe>\
             </body></html>",
        )
        .unwrap();

        let initial_generation = page.renderer_source_generation();
        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 globalThis.__liveSandbox = target.sandbox;\
                 target.setAttribute('sandbox', 'allow-same-origin');\
                 return String(__liveSandbox === target.sandbox) + ':' +\
                   __liveSandbox.value + ':' +\
                   __liveSandbox.contains('allow-same-origin'); })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:allow-same-origin:true".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);
        let target_id = page.query_selector_all("#target").unwrap()[0].node_id;
        assert!(
            page.computed_style(target_id)
                .contains(&("width".to_owned(), "140px".to_owned()))
        );

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 __liveSandbox.add('allow-forms');\
                 return target.getAttribute('sandbox') + ':' +\
                   String(__liveSandbox === target.sandbox) + ':' +\
                   Array.from(target.sandbox).join(','); })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String(
                "allow-same-origin allow-forms:true:allow-same-origin,allow-forms".to_owned()
            )
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 2);
        assert!(
            page.computed_style(target_id)
                .contains(&("height".to_owned(), "30px".to_owned()))
        );
    }

    #[test]
    fn live_inline_style_reflects_attributes_and_advances_one_source_revision_per_write() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///inline-style-mutate.html",
            "<html><body>\
               <div id='target' style='display: none'>target</div>\
             </body></html>",
        )
        .unwrap();

        let initial_generation = page.renderer_source_generation();
        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 globalThis.__liveStyle = target.style;\
                 target.setAttribute(\
                   'style',\
                   'display: block; width: 140px; height: 20px'\
                 );\
                 return String(__liveStyle === target.style) + ':' +\
                   __liveStyle.getPropertyValue('width') + ':' +\
                   __liveStyle.display; })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:140px:block".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);
        let target_id = page.query_selector_all("#target").unwrap()[0].node_id;
        assert!(
            page.computed_style(target_id)
                .contains(&("width".to_owned(), "140px".to_owned()))
        );

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 __liveStyle.setProperty('height', '30px');\
                 return String(__liveStyle === target.style) + ':' +\
                   target.getAttribute('style') + ':' + target.style.height; })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:display: block; width: 140px; height: 30px;:30px".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 2);
        assert!(
            page.computed_style(target_id)
                .contains(&("height".to_owned(), "30px".to_owned()))
        );
    }

    #[test]
    fn live_attributes_reflect_and_attr_value_advances_one_source_revision() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///attributes-mutate.html",
            "<html><head><style>\
               #target { display: none; }\
               #target[data-layout='wide'] { display: block; width: 140px; height: 20px; }\
               #target[data-layout='tall'] { display: block; width: 140px; height: 30px; }\
             </style></head><body>\
               <div id='target' data-layout='compact'>target</div>\
             </body></html>",
        )
        .unwrap();

        let initial_generation = page.renderer_source_generation();
        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 globalThis.__liveAttributes = target.attributes;\
                 globalThis.__liveLayoutAttr = __liveAttributes.getNamedItem('data-layout');\
                 target.setAttribute('data-layout', 'wide');\
                 return String(__liveAttributes === target.attributes) + ':' +\
                   String(__liveLayoutAttr === target.attributes.getNamedItem('data-layout')) + ':' +\
                   __liveLayoutAttr.value + ':' + target.attributes[1].name; })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:true:wide:data-layout".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);
        let target_id = page.query_selector_all("#target").unwrap()[0].node_id;
        assert!(
            page.computed_style(target_id)
                .contains(&("width".to_owned(), "140px".to_owned()))
        );

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => { const target = document.querySelector('#target');\
                 __liveLayoutAttr.value = 'tall';\
                 return String(__liveAttributes === target.attributes) + ':' +\
                   String(__liveLayoutAttr === target.attributes['data-layout']) + ':' +\
                   target.getAttribute('data-layout') + ':' + __liveLayoutAttr.value; })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:true:tall:tall".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 2);
        assert!(
            page.computed_style(target_id)
                .contains(&("height".to_owned(), "30px".to_owned()))
        );
    }

    #[test]
    fn detached_attributes_attach_replace_remove_and_retain_value() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///detached-attributes.html",
            "<style>\
               #target[data-layout='tall'] { display: block; width: 140px; height: 30px; }\
             </style>\
             <div id='target' data-layout='wide'>target</div>\
             <div id='other' data-use='locked'>other</div>",
        )
        .unwrap();

        let initial_generation = page.renderer_source_generation();
        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => {\
                   const target = document.querySelector('#target');\
                   const attributes = target.attributes;\
                   const prior = attributes.getNamedItem('data-layout');\
                   const detached = document.createAttribute('data-layout');\
                   detached.value = 'tall';\
                   globalThis.__detachedAttributes = attributes;\
                   globalThis.__priorLayoutAttr = prior;\
                   globalThis.__detachedLayoutAttr = detached;\
                   const replaced = attributes.setNamedItem(detached);\
                   return [\
                     replaced === prior, prior.ownerElement === null, prior.value,\
                     detached.ownerElement === target, attributes.getNamedItem('data-layout') === detached,\
                     target.getAttribute('data-layout')\
                   ].join(':');\
                 })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:true:wide:true:true:tall".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => {\
                   const removed = __detachedAttributes.removeNamedItem('data-layout');\
                   return [\
                     removed === __detachedLayoutAttr, removed.ownerElement === null, removed.value,\
                     __detachedAttributes.getNamedItem('data-layout') === null\
                   ].join(':');\
                 })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:true:tall:true".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 2);

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => {\
                   const target = document.querySelector('#target');\
                   const replaced = __detachedAttributes.setNamedItem(__detachedLayoutAttr);\
                   const inUse = document.querySelector('#other').attributes.getNamedItem('data-use');\
                   let errorName = '';\
                   try { __detachedAttributes.setNamedItem(inUse); } catch (error) { errorName = error.name; }\
                   return [\
                     replaced === null, __detachedLayoutAttr.ownerElement === target,\
                     __detachedAttributes.getNamedItem('data-layout') === __detachedLayoutAttr,\
                     errorName\
                   ].join(':');\
                 })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:true:true:InUseAttributeError".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 3);

        let target_id = page.query_selector_all("#target").unwrap()[0].node_id;
        assert!(
            page.computed_style(target_id)
                .contains(&("width".to_owned(), "140px".to_owned()))
        );
        assert!(
            page.computed_style(target_id)
                .contains(&("height".to_owned(), "30px".to_owned()))
        );
        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => {\
                   document.querySelector('#target').removeAttribute('data-layout');\
                   return [\
                     __detachedLayoutAttr.ownerElement === null, __detachedLayoutAttr.value,\
                     __detachedAttributes.getNamedItem('data-layout') === null\
                   ].join(':');\
                 })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:tall:true".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 4);
    }

    #[test]
    fn live_structural_collections_retain_identity_and_static_queries_stay_static() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///live-collections.html",
            "<html><body>\
               <div id='root'><span id='first' class='item'>first</span></div>\
               <form id='form'><input id='first-control' name='first-control'></form>\
             </body></html>",
        )
        .unwrap();

        let initial_generation = page.renderer_source_generation();
        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => {\
                   const root = document.querySelector('#root');\
                   const form = document.querySelector('#form');\
                   globalThis.__liveChildNodes = root.childNodes;\
                   globalThis.__liveChildren = root.children;\
                   globalThis.__liveItems = root.getElementsByClassName('item');\
                   globalThis.__liveSpans = document.getElementsByTagName('span');\
                   globalThis.__staticItems = root.querySelectorAll('.item');\
                   globalThis.__liveForms = document.forms;\
                   globalThis.__liveFormElements = form.elements;\
                   const second = document.createElement('span');\
                   second.id = 'second';\
                   second.className = 'item';\
                   second.textContent = 'second';\
                   root.appendChild(second);\
                   return [\
                     __liveChildNodes === root.childNodes,\
                     __liveChildren === root.children,\
                     __liveItems === root.getElementsByClassName('item'),\
                     __liveSpans === document.getElementsByTagName('span'),\
                     __liveForms === document.forms,\
                     __liveChildNodes.length, __liveChildren.length,\
                     __liveItems.length, __liveSpans.length, __staticItems.length,\
                     __liveChildren.second === second, __liveChildren.namedItem('second') === second\
                   ].join(':');\
                 })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:true:true:true:true:2:2:2:2:1:true:true".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => {\
                   const form = document.querySelector('#form');\
                   const second = document.createElement('input');\
                   second.id = 'second-control';\
                   second.name = 'second-control';\
                   form.appendChild(second);\
                   return [\
                     __liveFormElements === form.elements,\
                     __liveFormElements.length, form.length,\
                     __liveFormElements['second-control'] === second\
                   ].join(':');\
                 })()",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:2:2:true".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 2);
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
                 globalThis.__dynamicStyleSheet = style.sheet;
                 String(!!style.sheet) + ':' + loaded + ':' + getComputedStyle(document.querySelector('#target')).width",
                &mut page,
            )
            .unwrap();

        assert_eq!(value, JsValue::String("true:true:123px".to_owned()));
        let target = page.query_selector_all("#target").unwrap()[0].node_id;
        let computed = page.computed_style(target);
        assert!(computed.contains(&("width".to_owned(), "123px".to_owned())));
        assert_eq!(
            rt.evaluate_with_page_mut(
                "[\
                   __dynamicStyleSheet === document.styleSheets[0],\
                   __dynamicStyleSheet === document.querySelector('style').sheet,\
                   __dynamicStyleSheet.cssRules.length,\
                   __dynamicStyleSheet.cssRules[0].style.getPropertyValue('width')\
                 ].join(':')",
                &mut page,
            )
            .unwrap(),
            JsValue::String("true:true:1:123px".to_owned())
        );
    }

    #[test]
    fn live_cssom_objects_retain_identity_and_reflect_style_element_mutation() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///live-cssom.html",
            "<html><head><style id='author'>#target { display: block; width: 80px; }</style></head>\
             <body><div id='target'>target</div></body></html>",
        )
        .unwrap();

        assert_eq!(
            rt.evaluate_with_page_mut(
                "(() => {\
                   globalThis.__styleSheets = document.styleSheets;\
                   globalThis.__authorSheet = document.styleSheets[0];\
                   globalThis.__authorRules = __authorSheet.cssRules;\
                   globalThis.__authorRule = __authorRules[0];\
                   globalThis.__authorDeclarations = __authorRule.style;\
                   return document.querySelector('#author').sheet === __authorSheet;\
                 })()",
                &mut page,
            )
            .unwrap(),
            JsValue::Bool(true)
        );

        let initial_generation = page.renderer_source_generation();
        assert_eq!(
            rt.evaluate_with_page_mut(
                "document.querySelector('#author').textContent =\
                   '#target { display: block; width: 140px; height: 30px; }'",
                &mut page,
            )
            .unwrap(),
            JsValue::String("#target { display: block; width: 140px; height: 30px; }".to_owned())
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);

        assert_eq!(
            rt.evaluate_with_page_mut(
                "[\
                   __styleSheets === document.styleSheets,\
                   __authorSheet === document.styleSheets[0],\
                   __authorSheet === document.querySelector('#author').sheet,\
                   __authorRules === __authorSheet.cssRules,\
                   __authorRule === __authorRules[0],\
                   __authorDeclarations === __authorRule.style,\
                   __authorRule.selectorText,\
                   __authorDeclarations.getPropertyValue('width'),\
                   __authorDeclarations.getPropertyValue('height'),\
                   getComputedStyle(document.querySelector('#target')).width,\
                   getComputedStyle(document.querySelector('#target')).height\
                 ].join(':')",
                &mut page,
            )
            .unwrap(),
            JsValue::String(
                "true:true:true:true:true:true:#target:140px:30px:140px:30px".to_owned()
            )
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);
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
    fn page_focus_transition_uses_shared_order_and_persists_active_element() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///focus-order.html",
            "<html><body><input id='first'><input id='second'></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "(() => {\
                   const first = document.querySelector('#first');\
                   const second = document.querySelector('#second');\
                   first.focus();\
                   const events = [];\
                   for (const element of [first, second]) {\
                     for (const type of ['focusout', 'focusin', 'blur', 'focus']) {\
                       element.addEventListener(type, (event) => events.push(\
                         type + ':' + event.currentTarget.id + ':' + event.bubbles + ':' +\
                         (event.relatedTarget ? event.relatedTarget.id : '')\
                       ));\
                     }\
                   }\
                   second.focus();\
                   return events.join('>');\
                 })()",
                &mut page,
            )
            .unwrap();

        assert_eq!(
            value,
            JsValue::String(
                "focusout:first:true:second>focusin:second:true:first>blur:first:false:second>focus:second:false:first"
                    .to_owned()
            )
        );
        let second = page.query_selector_all("#second").unwrap()[0].node_id;
        assert_eq!(page.focused_element_node_id(), Some(second));

        let mut restored = JsRuntime::new().expect("restored runtime init");
        assert_eq!(
            restored
                .evaluate_with_page("document.activeElement.id", &page)
                .unwrap(),
            JsValue::String("second".to_owned())
        );
    }

    #[test]
    fn page_form_validation_dispatches_invalid_before_submit() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///validation.html",
            "<html><body><form id='form' action='done.html'><input id='first' name='first' required><input id='second' name='second' required><button id='go'>Go</button></form></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "(() => {\
                   const form = document.querySelector('#form');\
                   const first = document.querySelector('#first');\
                   const second = document.querySelector('#second');\
                   const go = document.querySelector('#go');\
                   const events = [];\
                   first.addEventListener('invalid', (event) => events.push('invalid:first:' + event.bubbles + ':' + event.cancelable));\
                   second.addEventListener('invalid', (event) => events.push('invalid:second:' + event.bubbles + ':' + event.cancelable));\
                   form.addEventListener('submit', () => events.push('submit'));\
                   form.requestSubmit(go);\
                   first.value = 'one';\
                   second.value = 'two';\
                   form.addEventListener('submit', (event) => event.preventDefault(), { once: true });\
                   form.requestSubmit(go);\
                   return events.join('>');\
                 })()",
                &mut page,
            )
            .unwrap();

        assert_eq!(
            value,
            JsValue::String("invalid:first:false:true>invalid:second:false:true>submit".to_owned())
        );
        assert!(rt.drain_navigation_actions().unwrap().is_empty());
    }

    #[test]
    fn page_range_selection_mutation_commits_and_restores() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///range.html",
            "<html><body><div id='root'><span id='a'>A</span><span id='b'>B</span><span id='c'>C</span></div></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "(() => {\
                   const root = document.querySelector('#root');\
                   const range = document.createRange();\
                   range.setStart(root, 1);\
                   range.setEnd(root, 2);\
                   const cloned = range.cloneContents().innerHTML;\
                   const selection = getSelection();\
                   selection.addRange(range);\
                   const before = [selection.type, selection.direction, selection.rangeCount, selection.toString()].join(':');\
                   selection.deleteFromDocument();\
                   return before + '|' + cloned + '|' + selection.type + ':' + selection.anchorOffset + ':' + root.children.length;\
                 })()",
                &mut page,
            )
            .unwrap();

        assert_eq!(
            value,
            JsValue::String("Range:forward:1:B|<span id=\"b\">B</span>|Caret:1:2".to_owned())
        );
        assert!(page.query_selector_all("#b").unwrap().is_empty());
        let root = page.query_selector_all("#root").unwrap()[0].node_id;
        assert_eq!(
            page.selection(),
            Some(crate::page::PageSelection {
                anchor_node_id: root,
                anchor_offset: 1,
                focus_node_id: root,
                focus_offset: 1,
            })
        );

        let mut restored = JsRuntime::new().expect("restored runtime init");
        assert_eq!(
            restored
                .evaluate_with_page(
                    "getSelection().type + ':' + getSelection().anchorNode.id + ':' + getSelection().anchorOffset",
                    &page,
                )
                .unwrap(),
            JsValue::String("Caret:root:1".to_owned())
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
                    submitter_node_id: Some(page.query_selector_all("#go").unwrap()[0].node_id),
                    submitter_id: Some("go".to_owned()),
                    action: "file:///nav/submit.html".to_owned(),
                    method: "get".to_owned(),
                    enctype: "application/x-www-form-urlencoded".to_owned(),
                },
            ]
        );
        assert_eq!(rt.drain_navigation_actions().unwrap(), Vec::new());
    }

    #[test]
    fn page_navigation_action_queue_retains_only_limit_and_overflow_marker() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "https://queue.test/start",
            "<!doctype html><title>Queue</title>",
        )
        .unwrap();
        rt.evaluate_with_page_mut(
            "for (let i = 0; i < 10000; i++) location.assign('/next-' + i)",
            &mut page,
        )
        .unwrap();

        let actions = rt.drain_navigation_actions().unwrap();
        assert_eq!(actions.len(), 65);
        assert!(matches!(actions.last(), Some(JsNavigationAction::Overflow)));
    }

    #[test]
    fn page_request_submit_uses_submitter_and_honors_prevent_default() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///request-submit/index.html",
            "<html><body><form id='form' action='submit.html'><button id='go'>Go</button><button id='alt' formaction='alt.html' formmethod='post' formenctype='text/plain'>Alt</button></form></body></html>",
        )
        .unwrap();

        let value = rt
            .evaluate_with_page_mut(
                "(() => {\
                   const form = document.querySelector('#form');\
                   const alt = document.querySelector('#alt');\
                   const events = [];\
                   form.addEventListener('submit', (event) => { events.push(event.submitter.id); event.preventDefault(); }, { once: true });\
                   form.requestSubmit(alt);\
                   form.addEventListener('submit', (event) => events.push(event.submitter.id), { once: true });\
                   form.requestSubmit(alt);\
                   return events.join(',');\
                 })()",
                &mut page,
            )
            .unwrap();

        assert_eq!(value, JsValue::String("alt,alt".to_owned()));
        assert_eq!(
            rt.drain_navigation_actions().unwrap(),
            vec![JsNavigationAction::FormSubmit {
                form_id: "form".to_owned(),
                form_node_id: page.query_selector_all("#form").unwrap()[0].node_id,
                submitter_node_id: Some(page.query_selector_all("#alt").unwrap()[0].node_id),
                submitter_id: Some("alt".to_owned()),
                action: "file:///request-submit/alt.html".to_owned(),
                method: "post".to_owned(),
                enctype: "text/plain".to_owned(),
            }]
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
    fn parser_modules_defer_after_classics_and_checkpoint_microtasks() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "file:///parser-modules.html",
            "<style>#target[data-module='done'] { display: block; width: 140px; height: 30px; }</style>\
             <div id='target'>target</div>\
             <script>\
               globalThis.__moduleOrder = ['classic-before'];\
               queueMicrotask(() => __moduleOrder.push('classic-microtask'));\
               setTimeout(() => {\
                 __moduleOrder.push('classic-timer');\
                 queueMicrotask(() => __moduleOrder.push('classic-timer-microtask'));\
               }, 0);\
               const cancelledTimer = setTimeout(() => __moduleOrder.push('cancelled-timer'), 0);\
               clearTimeout(cancelledTimer);\
               const interval = setInterval(() => {\
                 __moduleOrder.push('interval');\
                 clearInterval(interval);\
               }, 0);\
               requestAnimationFrame((timestamp) => __moduleOrder.push(Number.isFinite(timestamp) ? 'animation-frame' : 'bad-frame'));\
               const cancelledFrame = requestAnimationFrame(() => __moduleOrder.push('cancelled-frame'));\
               cancelAnimationFrame(cancelledFrame);\
             </script>\
             <script type='module'>\
               __moduleOrder.push('module-one');\
               queueMicrotask(() => __moduleOrder.push('module-one-microtask'));\
               await Promise.resolve();\
               __moduleOrder.push('module-one-await');\
               setTimeout(() => __moduleOrder.push('module-timer'), 0);\
               document.querySelector('#target').setAttribute('data-module', 'done');\
               export const one = 1;\
             </script>\
             <script>__moduleOrder.push('classic-after');</script>\
             <script type='module'>__moduleOrder.push('module-two'); export const two = 2;</script>",
        )
        .unwrap();

        let initial_generation = page.renderer_source_generation();
        assert_eq!(rt.execute_page_scripts(&mut page).unwrap(), 4);
        assert_eq!(
            rt.evaluate_with_page("__moduleOrder.join(',')", &page)
                .unwrap(),
            JsValue::String(
                "classic-before,classic-microtask,classic-after,module-one,module-one-microtask,module-one-await,module-two,classic-timer,classic-timer-microtask,interval,animation-frame,module-timer"
                    .to_owned()
            )
        );
        assert_eq!(page.renderer_source_generation(), initial_generation + 1);
        let target_id = page.query_selector_all("#target").unwrap()[0].node_id;
        assert!(
            page.computed_style(target_id)
                .contains(&("width".to_owned(), "140px".to_owned()))
        );
        assert!(
            page.computed_style(target_id)
                .contains(&("height".to_owned(), "30px".to_owned()))
        );

        let mut unresolved = Page::from_html(
            "file:///unresolved-module.html",
            "<script type='module'>import './missing.js';</script>",
        )
        .unwrap();
        assert_eq!(
            rt.execute_page_scripts(&mut unresolved).unwrap_err().code(),
            codes::SCRIPT_EVAL
        );
        assert_eq!(
            rt.evaluate_with_page("1 + 1", &unresolved).unwrap(),
            JsValue::Int32(2)
        );
    }

    #[test]
    fn static_module_dependencies_use_bounded_file_graph_loader() {
        let directory = std::env::temp_dir().join(format!(
            "vixen-module-graph-{}-{}",
            std::process::id(),
            next_storage_session_id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("nested.js"),
            "globalThis.__moduleLoads = (globalThis.__moduleLoads || 0) + 1; export const value = 41;",
        )
        .unwrap();
        std::fs::write(
            directory.join("dependency.js"),
            "import { value } from './nested.js'; export const answer = value + 1;",
        )
        .unwrap();
        let page_url = url::Url::from_file_path(directory.join("page.html"))
            .unwrap()
            .to_string();
        let mut page = Page::from_html(
            page_url,
            "<title>initial</title><script type='module'>import { answer } from './dependency.js'; globalThis.__moduleAnswer = answer; document.title = `module-${answer}`;</script>",
        )
        .unwrap();
        let mut runtime = JsRuntime::new().unwrap();

        assert_eq!(runtime.execute_page_scripts(&mut page).unwrap(), 1);
        assert_eq!(
            runtime
                .evaluate_with_page("`${__moduleAnswer}:${__moduleLoads}`", &page)
                .unwrap(),
            JsValue::String("42:1".to_owned())
        );
        assert_eq!(page.document().title().as_deref(), Some("module-42"));
        let events = runtime.drain_network_events().unwrap();
        let request_ids = events
            .iter()
            .filter_map(|event| match event {
                JsNetworkEvent::Request { request_id, .. } => Some(request_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(request_ids.len(), 2);
        assert_ne!(request_ids[0], request_ids[1]);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, JsNetworkEvent::Response { status: 200, .. }))
                .count(),
            2
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn parser_import_map_resolves_bare_prefix_and_import_meta_specifiers() {
        let directory = std::env::temp_dir().join(format!(
            "vixen-import-map-{}-{}",
            std::process::id(),
            next_storage_session_id()
        ));
        let assets = directory.join("assets");
        std::fs::create_dir_all(assets.join("pkg")).unwrap();
        std::fs::write(assets.join("answer.js"), "export const answer = 40;").unwrap();
        std::fs::write(assets.join("pkg/value.js"), "export const value = 2;").unwrap();
        let page_url = url::Url::from_file_path(directory.join("page.html"))
            .unwrap()
            .to_string();
        let mut page = Page::from_html(
            page_url,
            "<title>initial</title><base href='./assets/'>\
             <script type='importmap'>{\"imports\":{\"answer\":\"./answer.js\",\"pkg/\":\"./pkg/\"}}</script>\
             <script type='module'>\
               import { answer } from 'answer';\
               import { value } from 'pkg/value.js';\
               globalThis.__mappedUrl = import.meta.resolve('answer');\
               document.title = `mapped-${answer + value}`;\
             </script>",
        )
        .unwrap();
        let mut runtime = JsRuntime::new().unwrap();

        assert_eq!(runtime.execute_page_scripts(&mut page).unwrap(), 1);
        assert_eq!(page.document().title().as_deref(), Some("mapped-42"));
        assert_eq!(
            runtime
                .evaluate_with_page("__mappedUrl.endsWith('/assets/answer.js')", &page)
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            runtime
                .drain_network_events()
                .unwrap()
                .iter()
                .filter(|event| matches!(event, JsNetworkEvent::Response { status: 200, .. }))
                .count(),
            2
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn delayed_dynamic_import_uses_retained_graph_and_import_map() {
        let directory = std::env::temp_dir().join(format!(
            "vixen-dynamic-import-{}-{}",
            std::process::id(),
            next_storage_session_id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("child.js"), "export const child = 41;").unwrap();
        std::fs::write(
            directory.join("lazy.js"),
            "import { child } from './child.js'; globalThis.__dynamicLoads = (globalThis.__dynamicLoads || 0) + 1; export const value = child + 1;",
        )
        .unwrap();
        let page_url = url::Url::from_file_path(directory.join("page.html"))
            .unwrap()
            .to_string();
        let mut page = Page::from_html(
            page_url,
            "<title>initial</title>\
             <script type='importmap'>{\"imports\":{\"lazy\":\"./lazy.js\"}}</script>\
             <script type='module'>\
               globalThis.__loadDynamic = () => import('lazy').then(module => module.value);\
               globalThis.__loadAttributed = () => import('./child.js', { with: { type: 'css' } });\
             </script>\
             <script type='module'>globalThis.__laterRoot = true;</script>",
        )
        .unwrap();
        let mut runtime = JsRuntime::new().unwrap();
        assert_eq!(runtime.execute_page_scripts(&mut page).unwrap(), 2);

        assert_eq!(
            runtime
                .evaluate_with_page_mut(
                    "__loadDynamic().then(value => { document.title = `dynamic-${value}`; return value; })",
                    &mut page,
                )
                .unwrap(),
            JsValue::Int32(42)
        );
        assert_eq!(page.document().title().as_deref(), Some("dynamic-42"));
        assert_eq!(
            runtime
                .evaluate_with_page_mut(
                    "__loadDynamic().then(value => `${value}:${__dynamicLoads}`)",
                    &mut page
                )
                .unwrap(),
            JsValue::String("42:1".to_owned())
        );
        assert_eq!(
            runtime
                .evaluate_with_page_mut(
                    "__loadAttributed().then(() => false, error => /import attributes/.test(error.message))",
                    &mut page,
                )
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            runtime
                .evaluate_with_page_mut(
                    "import('./child.js').then(module => module.child)",
                    &mut page,
                )
                .unwrap(),
            JsValue::Int32(41)
        );
        assert_eq!(
            runtime
                .drain_network_events()
                .unwrap()
                .iter()
                .filter(|event| matches!(event, JsNetworkEvent::Request { .. }))
                .count(),
            2
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn static_and_dynamic_json_modules_use_the_bounded_graph_loader() {
        let directory = std::env::temp_dir().join(format!(
            "vixen-json-module-{}-{}",
            std::process::id(),
            next_storage_session_id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("config.json"), r#"{"value":40}"#).unwrap();
        std::fs::write(directory.join("later.json"), r#"{"value":2}"#).unwrap();
        std::fs::write(directory.join("unknown.json"), r#"{"value":3}"#).unwrap();
        let page_url = url::Url::from_file_path(directory.join("page.html"))
            .unwrap()
            .to_string();
        let mut page = Page::from_html(
            page_url,
            "<script type='module'>\
               import config from './config.json' with { type: 'json' };\
               globalThis.__jsonStatic = config.value;\
               globalThis.__loadJson = () => import('./later.json', { with: { type: 'json' } }).then(module => module.default.value);\
               globalThis.__loadUnknownAttribute = () => import('./unknown.json', { with: { type: 'json', unsupported: 'yes' } });\
             </script>",
        )
        .unwrap();
        let mut runtime = JsRuntime::new().unwrap();

        assert_eq!(runtime.execute_page_scripts(&mut page).unwrap(), 1);
        assert_eq!(
            runtime.evaluate_with_page("__jsonStatic", &page).unwrap(),
            JsValue::Int32(40)
        );
        assert_eq!(
            runtime
                .evaluate_with_page_mut("__loadJson()", &mut page)
                .unwrap(),
            JsValue::Int32(2)
        );
        assert_eq!(
            runtime
                .evaluate_with_page_mut(
                    "__loadUnknownAttribute().then(() => false, error => /only type=json/.test(error.message))",
                    &mut page,
                )
                .unwrap(),
            JsValue::Bool(true)
        );
        assert_eq!(
            runtime
                .drain_network_events()
                .unwrap()
                .iter()
                .filter(|event| matches!(event, JsNetworkEvent::Request { .. }))
                .count(),
            2
        );
        assert_eq!(
            runtime
                .evaluate_with_page("__jsonStatic + 2", &page)
                .unwrap(),
            JsValue::Int32(42)
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn classic_and_automation_dynamic_imports_use_document_provenance() {
        let directory = std::env::temp_dir().join(format!(
            "vixen-classic-dynamic-import-{}-{}",
            std::process::id(),
            next_storage_session_id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("classic-child.js"),
            "export const value = 40;",
        )
        .unwrap();
        std::fs::write(
            directory.join("automation-child.js"),
            "export const value = 2;",
        )
        .unwrap();
        let page_url = url::Url::from_file_path(directory.join("page.html"))
            .unwrap()
            .to_string();
        let mut page = Page::from_html(
            page_url,
            "<script type='importmap'>{\"imports\":{\"classic-child\":\"./classic-child.js\",\"automation-child\":\"./automation-child.js\"}}</script>\
             <script>globalThis.__loadClassicChild = () => import('classic-child').then(module => module.value);</script>",
        )
        .unwrap();
        let mut runtime = JsRuntime::new().unwrap();

        assert_eq!(runtime.execute_page_scripts(&mut page).unwrap(), 1);
        assert_eq!(
            runtime
                .evaluate_with_page_mut("__loadClassicChild()", &mut page)
                .unwrap(),
            JsValue::Int32(40)
        );
        assert_eq!(
            runtime
                .evaluate_with_page_mut(
                    "import('automation-child').then(module => module.value)",
                    &mut page,
                )
                .unwrap(),
            JsValue::Int32(2)
        );
        let events = runtime.drain_network_events().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, JsNetworkEvent::Request { .. }))
                .count(),
            2
        );
        assert!(events.iter().all(|event| match event {
            JsNetworkEvent::Request { url, .. } | JsNetworkEvent::Response { url, .. } => {
                url.starts_with("file:")
            }
            _ => true,
        }));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn relative_fixture_realm_evaluates_but_keeps_dynamic_import_fail_closed() {
        let page = Page::from_html(
            "fixtures/dom/basic.html",
            "<!doctype html><title>Relative fixture</title>",
        )
        .unwrap();
        let mut runtime = JsRuntime::new().unwrap();

        assert_eq!(
            runtime.evaluate_with_page("20 + 22", &page).unwrap(),
            JsValue::Int32(42)
        );
        assert_eq!(
            runtime
                .evaluate_with_page(
                    "import('./unresolved.js').then(() => false, () => true)",
                    &page,
                )
                .unwrap(),
            JsValue::Bool(true)
        );
        assert!(runtime.drain_network_events().unwrap().is_empty());
    }

    #[test]
    fn dynamic_imports_share_the_static_graph_load_limit() {
        let directory = std::env::temp_dir().join(format!(
            "vixen-dynamic-limit-{}-{}",
            std::process::id(),
            next_storage_session_id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        for index in 0..=64 {
            std::fs::write(
                directory.join(format!("module-{index}.js")),
                format!("export const value = {index};"),
            )
            .unwrap();
        }
        let page_url = url::Url::from_file_path(directory.join("page.html"))
            .unwrap()
            .to_string();
        let mut page = Page::from_html(
            page_url,
            "<script type='module'>globalThis.__loadByIndex = index => import(`./module-${index}.js`).then(module => module.value);</script>",
        )
        .unwrap();
        let mut runtime = JsRuntime::new().unwrap();
        assert_eq!(runtime.execute_page_scripts(&mut page).unwrap(), 1);

        for index in 0..64 {
            assert_eq!(
                runtime
                    .evaluate_with_page_mut(&format!("__loadByIndex({index})"), &mut page)
                    .unwrap(),
                JsValue::Int32(index)
            );
        }
        assert_eq!(
            runtime
                .evaluate_with_page_mut(
                    "__loadByIndex(64).then(() => '', error => error.message)",
                    &mut page,
                )
                .unwrap(),
            JsValue::String("module graph exceeds 64 dependency loads".to_owned())
        );
        assert_eq!(
            runtime.evaluate_with_page("6 * 7", &page).unwrap(),
            JsValue::Int32(42)
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn external_multiple_and_late_import_maps_fail_closed() {
        let external = Page::from_html(
            "https://example.test/external.html",
            "<script type='importmap' src='/map.json'></script>",
        )
        .unwrap();
        let mut scripts = PageScriptRunner::new(&external, false);
        assert!(matches!(
            scripts.prepare_next(&external),
            Some(PreparedPageScript::ImportMap { error: Some(error), .. })
                if error.contains("external")
        ));

        let multiple = Page::from_html(
            "https://example.test/multiple.html",
            "<script type='importmap'>{\"imports\":{}}</script>\
             <script type='importmap'>{\"imports\":{}}</script>",
        )
        .unwrap();
        let mut scripts = PageScriptRunner::new(&multiple, false);
        assert!(matches!(
            scripts.prepare_next(&multiple),
            Some(PreparedPageScript::ImportMap { error: None, .. })
        ));
        assert!(matches!(
            scripts.prepare_next(&multiple),
            Some(PreparedPageScript::ImportMap { error: Some(error), .. })
                if error.contains("multiple")
        ));

        let late = Page::from_html(
            "https://example.test/late.html",
            "<script type='module'>import 'answer';</script>\
             <script type='importmap'>{\"imports\":{\"answer\":\"./answer.js\"}}</script>",
        )
        .unwrap();
        let mut scripts = PageScriptRunner::new(&late, false);
        assert!(matches!(
            scripts.prepare_next(&late),
            Some(PreparedPageScript::ImportMap { error: Some(error), .. })
                if error.contains("after module")
        ));
        assert!(matches!(
            scripts.prepare_next(&late),
            Some(PreparedPageScript::InlineModule { .. })
        ));

        let root = Page::from_html(
            "https://example.test/root.html",
            "<script type='importmap'>{\"imports\":{\"./root.js\":\"./wrong.js\"}}</script>\
             <script type='module' src='./root.js'></script>",
        )
        .unwrap();
        let mut scripts = PageScriptRunner::new(&root, false);
        assert!(matches!(
            scripts.prepare_next(&root),
            Some(PreparedPageScript::ImportMap { error: None, .. })
        ));
        assert!(matches!(
            scripts.prepare_next(&root),
            Some(PreparedPageScript::External(request))
                if request.url().as_str() == "https://example.test/root.js"
        ));

        let allowed = Page::from_html_with_headers(
            "https://example.test/csp-map.html",
            "<script type='importmap' nonce='ok'>{\"imports\":{}}</script>\
             <script type='module' nonce='ok'>export const value = 1;</script>",
            [("Content-Security-Policy", "script-src 'nonce-ok'")],
        )
        .unwrap();
        let mut scripts = PageScriptRunner::new(&allowed, false);
        assert!(matches!(
            scripts.prepare_next(&allowed),
            Some(PreparedPageScript::ImportMap { error: None, .. })
        ));
        assert!(matches!(
            scripts.prepare_next(&allowed),
            Some(PreparedPageScript::InlineModule { .. })
        ));

        let blocked = Page::from_html_with_headers(
            "https://example.test/csp-blocked-map.html",
            "<script type='importmap'>{\"imports\":{}}</script>",
            [("Content-Security-Policy", "script-src 'none'")],
        )
        .unwrap();
        let mut scripts = PageScriptRunner::new(&blocked, false);
        assert!(matches!(
            scripts.prepare_next(&blocked),
            Some(PreparedPageScript::Skip)
        ));
    }

    #[test]
    fn cross_origin_module_dependency_requires_cors_response() {
        let page = Page::from_html(
            "https://module-origin.example/page.html",
            "<script type='module' nonce='allowed'>import 'https://cross-origin.example/dependency.js';</script>",
        )
        .unwrap();
        let mut scripts = PageScriptRunner::new(&page, false);
        let PreparedPageScript::InlineModule { request, .. } = scripts.prepare_next(&page).unwrap()
        else {
            panic!("expected inline module");
        };
        let dependency = request
            .module_dependency(
                url::Url::parse("https://cross-origin.example/dependency.js").unwrap(),
            )
            .unwrap();
        let mut response = vixen_net::ByteResponse {
            body: b"export const value = 42;".to_vec(),
            headers: std::collections::BTreeMap::from([(
                "content-type".to_owned(),
                "text/javascript".to_owned(),
            )]),
            status: 200,
            final_url: dependency.url().to_string(),
            set_cookie: Vec::new(),
            redirects: 0,
            events: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
            from_cache: false,
            redirect_aliasable: true,
        };

        assert!(!dependency.sends_credentials(dependency.url()));
        assert_eq!(
            dependency.request_headers(dependency.url()),
            vec![(
                "origin".to_owned(),
                "https://module-origin.example".to_owned()
            )]
        );
        assert_eq!(
            dependency.response_blocked_reason(dependency.url(), &response),
            Some("cors")
        );
        response
            .headers
            .insert("access-control-allow-origin".to_owned(), "*".to_owned());
        assert_eq!(
            dependency.response_blocked_reason(dependency.url(), &response),
            None
        );

        let mut credentialed = dependency.clone();
        credentialed.module_credentials = ModuleCredentialsMode::Include;
        assert!(credentialed.sends_credentials(credentialed.url()));
        assert_eq!(
            credentialed.response_blocked_reason(credentialed.url(), &response),
            Some("cors")
        );
        response.headers.insert(
            "access-control-allow-origin".to_owned(),
            "https://module-origin.example".to_owned(),
        );
        response.headers.insert(
            "access-control-allow-credentials".to_owned(),
            "true".to_owned(),
        );
        assert_eq!(
            credentialed.response_blocked_reason(credentialed.url(), &response),
            None
        );
        assert!(
            credentialed
                .module_dependency(url::Url::parse("file:///secret.js").unwrap())
                .is_err()
        );
    }

    #[test]
    fn external_parser_module_uses_script_policy_and_document_realm() {
        let (base_url, network_config, server) = spawn_script_server(
            "vixen-module-success.com",
            "globalThis.__externalModule = 'ran'; export const answer = 42;",
            &[("Content-Type", "text/javascript; charset=utf-8")],
        );
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let html = format!(
            "<script>globalThis.__externalModule = 'classic';</script>\
             <script type='module' src='{base_url}/module.js'></script>"
        );
        let mut page = Page::from_html(format!("{base_url}/page.html"), &html).unwrap();

        assert_eq!(rt.execute_page_scripts(&mut page).unwrap(), 2);
        assert_eq!(
            rt.evaluate_with_page("__externalModule", &page).unwrap(),
            JsValue::String("ran".to_owned())
        );
        server.join().unwrap();
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
    fn fetch_exposes_a_chunked_readable_stream_and_body_use_state() {
        let (url, network_config, server) =
            spawn_fetch_server("vixen-fetch-stream.com", "stream body");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expression = format!(
            r#"fetch({url:?}).then(async (response) => {{
              const isStream = response.body instanceof ReadableStream;
              const reader = response.body.getReader();
              const chunks = [];
              for (;;) {{
                const item = await reader.read();
                if (item.done) break;
                chunks.push(item.value);
              }}
              const bytes = new Uint8Array(chunks.reduce((sum, chunk) => sum + chunk.length, 0));
              let offset = 0;
              for (const chunk of chunks) {{ bytes.set(chunk, offset); offset += chunk.length; }}
              const secondReadRejected = await response.text().then(() => false, (error) => error instanceof TypeError);
              return isStream + ':' + response.bodyUsed + ':' + chunks.length + ':' + new TextDecoder().decode(bytes) + ':' + secondReadRejected;
            }})"#
        );

        assert_eq!(
            rt.evaluate(&expression).unwrap(),
            JsValue::String("true:true:1:stream body:true".to_owned())
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_resolves_at_policy_accepted_head_and_streams_before_completion() {
        use std::time::{Duration, Instant};

        let (url, network_config, headers, release_first, first_sent, release_finish, server) =
            spawn_gated_streaming_fetch_server("vixen-fetch-live-stream.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let started = Instant::now();
        let expression = format!(
            r#"fetch({url:?}).then((response) => {{
              globalThis.__liveResponse = response;
              return response.status + ':' + response.headers.get('x-vixen-stream') + ':' + response.bodyUsed;
            }})"#
        );

        assert_eq!(
            rt.evaluate(&expression).unwrap(),
            JsValue::String("200:yes:false".to_owned())
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        headers
            .recv_timeout(Duration::from_secs(1))
            .expect("server published response headers");
        let head_events = rt.drain_network_events().unwrap();
        assert!(matches!(
            head_events.as_slice(),
            [JsNetworkEvent::Request { request_id, .. }, JsNetworkEvent::Response { request_id: response_id, .. }]
                if request_id == response_id
        ));

        release_first.send(()).unwrap();
        first_sent
            .recv_timeout(Duration::from_secs(1))
            .expect("server published first body chunk");
        assert_eq!(
            rt.evaluate(
                r#"globalThis.__liveReader = __liveResponse.body.getReader();
                   __liveReader.read().then((item) => new TextDecoder().decode(item.value) + ':' + item.done)"#,
            )
            .unwrap(),
            JsValue::String("first-:false".to_owned())
        );

        release_finish.send(()).unwrap();
        server.join().unwrap();
        assert_eq!(
            rt.evaluate(
                r#"__liveReader.read().then((item) => new TextDecoder().decode(item.value) + ':' + item.done)"#,
            )
            .unwrap(),
            JsValue::String("second:false".to_owned())
        );
        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(
            rt.evaluate("__liveReader.read().then((item) => item.done)")
                .unwrap(),
            JsValue::Bool(true)
        );
        let terminal_events = rt.drain_network_events().unwrap();
        assert!(matches!(
            terminal_events.as_slice(),
            [JsNetworkEvent::Progress { request_id, .. }, JsNetworkEvent::Completed { request_id: completed_id, .. }]
                if request_id == completed_id
                    && head_events.iter().all(|event| match event {
                        JsNetworkEvent::Request { request_id: head_id, .. }
                        | JsNetworkEvent::Response { request_id: head_id, .. } => head_id == request_id,
                        _ => false,
                    })
        ));
    }

    #[test]
    fn abort_after_fetch_resolution_cancels_the_owned_body_stream() {
        use std::sync::atomic::Ordering;

        let (url, network_config, disconnected, server) =
            spawn_stalled_fetch_server("vixen-fetch-body-abort.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expression = format!(
            r#"globalThis.__bodyController = new AbortController();
               fetch({url:?}, {{ signal: __bodyController.signal }}).then((response) => {{
                 globalThis.__bodyResponse = response;
                 return response.status;
               }})"#
        );
        assert_eq!(rt.evaluate(&expression).unwrap(), JsValue::Int32(200));
        assert_eq!(
            rt.evaluate(
                r#"globalThis.__bodyAbortReason = new Error('body abort');
                   __bodyController.abort(__bodyAbortReason);
                   __bodyResponse.text().then(() => false, (error) => error === __bodyAbortReason)"#,
            )
            .unwrap(),
            JsValue::Bool(true)
        );

        server.join().unwrap();
        assert!(disconnected.load(Ordering::SeqCst));
        let events = rt.drain_network_events().unwrap();
        assert!(matches!(
            events.as_slice(),
            [
                JsNetworkEvent::Request { request_id, .. },
                JsNetworkEvent::Response { request_id: response_id, .. },
                JsNetworkEvent::Failure { request_id: failure_id, blocked_reason: Some(reason), .. },
            ] if request_id == response_id && request_id == failure_id && reason == "aborted"
        ));
    }

    #[test]
    fn fetch_integrity_keeps_response_buffered_until_verification() {
        use std::time::{Duration, Instant};

        let delay = Duration::from_millis(100);
        let digest = "sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";
        let (url, network_config, server) =
            spawn_delayed_fetch_server("vixen-fetch-sri-buffered.com", "abc", delay);
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expression = format!(
            "fetch({url:?}, {{ integrity: {digest:?} }}).then((response) => response.status)"
        );
        let started = Instant::now();

        assert_eq!(rt.evaluate(&expression).unwrap(), JsValue::Int32(200));
        assert!(started.elapsed() >= Duration::from_millis(75));
        server.join().unwrap();
    }

    #[test]
    fn preaborted_fetch_rejects_without_transport_and_keeps_first_reason() {
        let mut rt = JsRuntime::new().expect("engine init");

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                  const controller = new AbortController();
                  let events = 0;
                  controller.signal.addEventListener('abort', () => events++);
                  const reason = new Error('first');
                  controller.abort(reason);
                  controller.abort(new Error('second'));
                  return fetch('http://vixen-preaborted-fetch.com/payload', { signal: controller.signal })
                    .then(() => 'resolved', (error) => [error === reason, controller.signal.reason === reason, events].join(':'));
                })()"#,
            )
            .unwrap(),
            JsValue::String("true:true:1".to_owned())
        );
        assert!(rt.drain_network_events().unwrap().is_empty());
    }

    #[test]
    fn active_fetch_abort_disconnects_transport_and_keeps_signal_reason() {
        use std::sync::atomic::Ordering;

        let (url, network_config, disconnected, server) =
            spawn_stalled_fetch_server("vixen-fetch-active-abort.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expression = format!(
            r#"(() => {{
              const controller = new AbortController();
              const reason = new Error('active abort');
              const pending = fetch({url:?}, {{ signal: controller.signal }});
              const deadline = Date.now() + 75;
              while (Date.now() < deadline) {{}}
              controller.abort(reason);
              return pending.then(() => false, (error) => error === reason);
            }})()"#
        );

        assert_eq!(rt.evaluate(&expression).unwrap(), JsValue::Bool(true));
        server.join().unwrap();
        assert!(disconnected.load(Ordering::SeqCst));
        let events = rt.drain_network_events().unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], JsNetworkEvent::Request { .. }));
        assert!(matches!(events[1], JsNetworkEvent::Failure { .. }));
    }

    #[test]
    fn xhr_abort_disconnects_transport_without_late_terminal_events() {
        use std::sync::atomic::Ordering;

        let (url, network_config, disconnected, server) =
            spawn_stalled_fetch_server("vixen-xhr-active-abort.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expression = format!(
            r#"(() => {{
              const xhr = new XMLHttpRequest();
              const events = [];
              for (const type of ['readystatechange', 'loadstart', 'abort', 'loadend', 'load', 'error']) {{
                xhr.addEventListener(type, () => events.push(type));
              }}
              xhr.open('GET', {url:?});
              events.length = 0;
              xhr.send();
              const deadline = Date.now() + 75;
              while (Date.now() < deadline) {{}}
              xhr.abort();
              return Promise.resolve().then(() => events.join(','));
            }})()"#
        );

        assert_eq!(
            rt.evaluate(&expression).unwrap(),
            JsValue::String("loadstart,readystatechange,abort,loadend".to_owned())
        );
        server.join().unwrap();
        assert!(disconnected.load(Ordering::SeqCst));
    }

    #[test]
    fn fetch_records_stable_network_events() {
        let (url, network_config, server) =
            spawn_fetch_server("vixen-fetch-events.com", "event body");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expr = format!("fetch({url:?}).then((response) => response.text())");

        assert_eq!(
            rt.evaluate(&expr).unwrap(),
            JsValue::String("event body".to_owned())
        );
        let events = rt.drain_network_events().unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(
            events[0],
            JsNetworkEvent::Request {
                request_id: "fetch-1".to_owned(),
                url: url.clone(),
                method: "GET".to_owned(),
            }
        );
        assert_eq!(
            events[1],
            JsNetworkEvent::Response {
                request_id: "fetch-1".to_owned(),
                url: url.clone(),
                status: 200,
            }
        );
        assert_eq!(
            events[2],
            JsNetworkEvent::Progress {
                request_id: "fetch-1".to_owned(),
                url: url.clone(),
                chunk_bytes: 10,
                loaded_bytes: 10,
                total_bytes: Some(10),
            }
        );
        assert_eq!(
            events[3],
            JsNetworkEvent::Completed {
                request_id: "fetch-1".to_owned(),
                url: url.clone(),
                body_bytes: 10,
            }
        );
        assert_eq!(rt.drain_network_events().unwrap(), Vec::new());
        server.join().unwrap();
    }

    #[test]
    fn fetch_records_failure_network_events() {
        let mut rt = JsRuntime::new().expect("engine init");
        assert_eq!(
            rt.evaluate(
                "fetch('http://127.0.0.1:9/').then(() => false, (err) => /URL rejected/.test(err.message))"
            )
            .unwrap(),
            JsValue::Bool(true)
        );
        let events = rt.drain_network_events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            JsNetworkEvent::Request {
                request_id: "fetch-1".to_owned(),
                url: "http://127.0.0.1:9/".to_owned(),
                method: "GET".to_owned(),
            }
        );
        let JsNetworkEvent::Failure {
            request_id,
            url,
            error_text,
            blocked_reason,
        } = &events[1]
        else {
            panic!("expected failure event: {:?}", events[1]);
        };
        assert_eq!(request_id, "fetch-1");
        assert_eq!(url, "http://127.0.0.1:9/");
        assert!(error_text.contains("URL rejected by policy: blocked host"));
        assert_eq!(blocked_reason.as_deref(), Some("url-policy"));
    }

    #[test]
    fn fetch_cors_blocks_cross_origin_without_allow_origin() {
        let (url, network_config, server) =
            spawn_fetch_server("vixen-fetch-cors-block.com", "blocked");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut page = Page::from_html("http://source.test/page", "<main></main>").unwrap();
        let expr = format!(
            "fetch({url:?}).then(() => false, (err) => err instanceof TypeError && /blocked by CORS/.test(err.message))"
        );

        assert_eq!(
            rt.evaluate_with_page_mut(&expr, &mut page).unwrap(),
            JsValue::Bool(true)
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_rejects_disallowed_response_head_before_body_completion() {
        use std::sync::atomic::Ordering;

        let (url, network_config, disconnected, server) =
            spawn_stalled_fetch_server("vixen-fetch-cors-head-block.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut page = Page::from_html("http://source.test/page", "<main></main>").unwrap();
        let expression = format!(
            "fetch({url:?}).then(() => false, (error) => /blocked by CORS/.test(error.message))"
        );

        assert_eq!(
            rt.evaluate_with_page_mut(&expression, &mut page).unwrap(),
            JsValue::Bool(true)
        );
        server.join().unwrap();
        assert!(disconnected.load(Ordering::SeqCst));
        assert!(matches!(
            rt.drain_network_events().unwrap().as_slice(),
            [
                JsNetworkEvent::Request { .. },
                JsNetworkEvent::Failure {
                    blocked_reason: Some(reason),
                    ..
                }
            ] if reason == "cors"
        ));
    }

    #[test]
    fn fetch_cors_allows_cross_origin_and_filters_headers() {
        let (url, network_config, server) =
            spawn_cors_server("vixen-fetch-cors-allow.com", "http://source.test");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut page = Page::from_html("http://source.test/page", "<main></main>").unwrap();
        let expr = format!(
            "fetch({url:?}).then((response) => response.text().then((body) => response.type + ':' + response.status + ':' + response.headers.get('content-type') + ':' + response.headers.get('x-vixen-test') + ':' + response.headers.get('x-hidden') + ':' + body))"
        );

        assert_eq!(
            rt.evaluate_with_page_mut(&expr, &mut page).unwrap(),
            JsValue::String("cors:200:text/plain:yes:null:cors-ok".to_owned())
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_cors_preflights_non_safelisted_headers() {
        let (url, network_config, server) = spawn_preflight_server("vixen-fetch-preflight.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut page = Page::from_html("http://source.test/page", "<main></main>").unwrap();
        let expr = format!(
            "fetch({url:?}, {{ method: 'POST', headers: {{ 'X-Vixen-Custom': 'yes' }}, body: 'preflight body' }}).then((response) => response.text())"
        );

        assert_eq!(
            rt.evaluate_with_page_mut(&expr, &mut page).unwrap(),
            JsValue::String("preflight-ok".to_owned())
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_preflight_cache_reuses_success() {
        use std::sync::atomic::Ordering;

        let (url, network_config, preflights, server) =
            spawn_cached_preflight_server("vixen-fetch-preflight-cache.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut page = Page::from_html("http://source.test/page", "<main></main>").unwrap();
        let expr = format!(
            "fetch({url:?}, {{ method: 'POST', headers: {{ 'X-Vixen-Custom': 'yes' }} }}).then(() => fetch({url:?}, {{ method: 'POST', headers: {{ 'X-Vixen-Custom': 'again' }} }})).then((response) => response.text())"
        );

        assert_eq!(
            rt.evaluate_with_page_mut(&expr, &mut page).unwrap(),
            JsValue::String("cached-preflight-ok".to_owned())
        );
        server.join().unwrap();
        assert_eq!(preflights.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fetch_extra_header_participates_in_preflight() {
        let (url, network_config, server) =
            spawn_preflight_server("vixen-fetch-extra-header-preflight.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        rt.set_extra_http_headers(vec![("X-Vixen-Custom".to_owned(), "yes".to_owned())]);
        let mut page = Page::from_html("http://source.test/page", "<main></main>").unwrap();
        let expr = format!(
            "fetch({url:?}, {{ method: 'POST', body: 'preflight body' }}).then((response) => response.text())"
        );

        assert_eq!(
            rt.evaluate_with_page_mut(&expr, &mut page).unwrap(),
            JsValue::String("preflight-ok".to_owned())
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_credentials_require_allow_credentials() {
        let (url, network_config, server) =
            spawn_cors_server("vixen-fetch-cors-credentials.com", "http://source.test");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut page = Page::from_html("http://source.test/page", "<main></main>").unwrap();
        let expr = format!(
            "fetch({url:?}, {{ credentials: 'include' }}).then(() => false, (error) => /Allow-Credentials/.test(error.message))"
        );

        assert_eq!(
            rt.evaluate_with_page_mut(&expr, &mut page).unwrap(),
            JsValue::Bool(true)
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_integrity_accepts_match_and_reports_mismatch() {
        let digest = "sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";
        let (ok_url, ok_config, ok_server) = spawn_fetch_server("vixen-fetch-sri-ok.com", "abc");
        let mut ok_runtime = JsRuntime::with_network_config(ok_config).expect("engine init");
        let ok_expr = format!(
            "fetch({ok_url:?}, {{ integrity: {digest:?} }}).then((response) => response.text())"
        );
        assert_eq!(
            ok_runtime.evaluate(&ok_expr).unwrap(),
            JsValue::String("abc".to_owned())
        );
        ok_server.join().unwrap();

        let (bad_url, bad_config, bad_server) =
            spawn_fetch_server("vixen-fetch-sri-bad.com", "tampered");
        let mut bad_runtime = JsRuntime::with_network_config(bad_config).expect("engine init");
        let bad_expr = format!(
            "fetch({bad_url:?}, {{ integrity: {digest:?} }}).then(() => false, (error) => /integrity mismatch/.test(error.message))"
        );
        assert_eq!(
            bad_runtime.evaluate(&bad_expr).unwrap(),
            JsValue::Bool(true)
        );
        let events = bad_runtime.drain_network_events().unwrap();
        assert!(matches!(
            events.as_slice(),
            [
                JsNetworkEvent::Request { .. },
                JsNetworkEvent::Failure {
                    blocked_reason: Some(reason),
                    ..
                }
            ] if reason == "integrity"
        ));
        bad_server.join().unwrap();
    }

    #[test]
    fn fetch_same_origin_mode_blocks_cross_origin_before_network() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html("http://source.test/page", "<main></main>").unwrap();

        assert_eq!(
            rt.evaluate_with_page_mut("fetch('http://example.com/payload', { mode: 'same-origin' }).then(() => false, (err) => err instanceof TypeError && /mode same-origin/.test(err.message))", &mut page)
                .unwrap(),
            JsValue::Bool(true)
        );
    }

    #[test]
    fn fetch_no_cors_cross_origin_returns_opaque_response() {
        let (url, network_config, server) =
            spawn_fetch_server("vixen-fetch-no-cors.com", "opaque body");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut page = Page::from_html("http://source.test/page", "<main></main>").unwrap();
        let expr = format!(
            "fetch({url:?}, {{ mode: 'no-cors' }}).then((response) => response.text().then((body) => response.type + ':' + response.status + ':' + response.url + ':' + response.headers.get('content-type') + ':' + body))"
        );

        assert_eq!(
            rt.evaluate_with_page_mut(&expr, &mut page).unwrap(),
            JsValue::String("opaque:0::null:".to_owned())
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_sends_allowed_request_headers() {
        let (url, network_config, server) = spawn_header_echo_server("vixen-fetch-headers.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expr = format!(
            "fetch({url:?}, {{ headers: {{ 'X-Vixen-Test': 'yes', 'Host': 'evil.example' }} }}).then((response) => response.text())"
        );

        assert_eq!(
            rt.evaluate(&expr).unwrap(),
            JsValue::String("header-ok".to_owned())
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_sends_request_body_for_unsafe_methods() {
        let (url, network_config, server) = spawn_body_echo_server("vixen-fetch-body.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expr = format!(
            "fetch({url:?}, {{ method: 'POST', body: 'hello body' }}).then((response) => response.text())"
        );

        assert_eq!(
            rt.evaluate(&expr).unwrap(),
            JsValue::String("post:text/plain;charset=utf-8:hello body".to_owned())
        );
        server.join().unwrap();
    }

    #[test]
    fn xhr_posts_body_and_reads_response_headers() {
        let (url, network_config, server) = spawn_body_echo_server("vixen-xhr-body.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expr = format!(
            r#"new Promise((resolve) => {{
              const xhr = new XMLHttpRequest();
              const states = [];
              xhr.onreadystatechange = () => states.push(xhr.readyState);
              xhr.onload = () => resolve(xhr.status + ':' + xhr.getResponseHeader('content-type') + ':' + states.join(',') + ':' + xhr.responseText);
              xhr.open('POST', {url:?});
              xhr.send('xhr body');
            }})"#
        );

        assert_eq!(
            rt.evaluate(&expr).unwrap(),
            JsValue::String(
                "200:text/plain:1,2,3,4:post:text/plain;charset=utf-8:xhr body".to_owned()
            )
        );
        server.join().unwrap();
    }

    #[test]
    fn xhr_reports_bounded_download_and_upload_progress() {
        let (url, network_config, server) = spawn_body_echo_server("vixen-xhr-progress.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expression = format!(
            r#"new Promise((resolve) => {{
              const xhr = new XMLHttpRequest();
              const download = [];
              const upload = [];
              xhr.onprogress = (event) => download.push([event.lengthComputable, event.loaded, event.total].join('/'));
              xhr.upload.onprogress = (event) => upload.push([event.lengthComputable, event.loaded, event.total].join('/'));
              xhr.onloadend = () => resolve(upload.join(',') + ':' + download.join(',') + ':' + xhr.responseText);
              xhr.open('POST', {url:?});
              xhr.send('abc');
            }})"#
        );

        assert_eq!(
            rt.evaluate(&expression).unwrap(),
            JsValue::String("true/3/3:true/33/33:post:text/plain;charset=utf-8:abc".to_owned())
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_applies_referrer_policy_header() {
        let (url, network_config, server) = spawn_referrer_echo_server("vixen-fetch-referrer.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let mut page =
            Page::from_html("http://source.test/path?q=1#frag", "<main></main>").unwrap();
        let expr = format!(
            "fetch({url:?}, {{ referrerPolicy: 'unsafe-url' }}).then((response) => response.text())"
        );

        assert_eq!(
            rt.evaluate_with_page_mut(&expr, &mut page).unwrap(),
            JsValue::String("referrer-ok".to_owned())
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_redirect_manual_returns_redirect_response() {
        let (url, network_config, server) =
            spawn_redirect_server("vixen-fetch-redirect-manual.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expr = format!(
            "fetch(new Request({url:?}, {{ redirect: 'manual' }})).then((response) => response.text().then((body) => response.status + ':' + response.redirected + ':' + response.url + ':' + response.headers.get('location') + ':' + body))"
        );

        assert_eq!(
            rt.evaluate(&expr).unwrap(),
            JsValue::String(format!("302:false:{url}:/target:"))
        );
        server.join().unwrap();
    }

    #[test]
    fn fetch_redirect_error_rejects_redirect_response() {
        let (url, network_config, server) = spawn_redirect_server("vixen-fetch-redirect-error.com");
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expr = format!(
            "fetch({url:?}, {{ redirect: 'error' }}).then(() => false, (err) => err instanceof TypeError && /redirect disallowed/.test(err.message))"
        );

        assert_eq!(rt.evaluate(&expr).unwrap(), JsValue::Bool(true));
        server.join().unwrap();
    }

    #[test]
    fn fetch_reuses_fresh_permanent_redirect_alias_without_transport() {
        use std::sync::atomic::Ordering;

        let (url, target_url, network_config, requests, server) =
            spawn_cacheable_redirect_server("vixen-fetch-cache-alias.com");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-fetch-cache-alias-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
            .expect("engine init");
        let expression = format!(
            "fetch({url:?}).then((response) => response.text().then((body) => response.url + ':' + response.redirected + ':' + body))"
        );
        let expected = JsValue::String(format!("{target_url}:true:alias-target"));

        assert_eq!(rt.evaluate(&expression).unwrap(), expected);
        rt.drain_network_events().unwrap();
        assert_eq!(rt.evaluate(&expression).unwrap(), expected);
        let events = rt.drain_network_events().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            JsNetworkEvent::Redirect { from, to, status, .. }
                if from == &url && to == &target_url && *status == 301
        )));

        server.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        drop(rt);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fetch_rejects_invalid_redirect_mode() {
        let mut rt = JsRuntime::new().expect("engine init");

        assert_eq!(
            rt.evaluate("fetch('http://vixen-invalid-redirect-mode.com/payload', { redirect: 'elsewhere' }).then(() => false, (err) => err instanceof TypeError && /unsupported fetch redirect mode: elsewhere/.test(err.message))")
                .unwrap(),
            JsValue::Bool(true)
        );
    }

    #[test]
    fn fetch_cookies_persist_through_profile_store() {
        let (base_url, network_config, server) = spawn_cookie_echo_server("vixen-cookie-store.com");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-cookie-store-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);

        {
            let mut rt =
                JsRuntime::with_network_config_and_storage_path(network_config.clone(), &path)
                    .expect("engine init");
            let set_url = format!("{base_url}/set");
            let expr = format!("fetch({set_url:?}).then((response) => response.text())");
            assert_eq!(
                rt.evaluate(&expr).unwrap(),
                JsValue::String("set".to_owned())
            );
        }

        {
            let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
                .expect("engine init");
            let echo_url = format!("{base_url}/echo");
            let expr = format!("fetch({echo_url:?}).then((response) => response.text())");
            assert_eq!(
                rt.evaluate(&expr).unwrap(),
                JsValue::String("sid=abc".to_owned())
            );
        }

        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fetch_credentials_omit_does_not_send_profile_cookies() {
        let (base_url, network_config, server) =
            spawn_cookie_echo_server("vixen-cookie-credentials-omit.com");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-cookie-credentials-omit-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);

        {
            let mut rt =
                JsRuntime::with_network_config_and_storage_path(network_config.clone(), &path)
                    .expect("engine init");
            let set_url = format!("{base_url}/set");
            let expr = format!("fetch({set_url:?}).then((response) => response.text())");
            assert_eq!(
                rt.evaluate(&expr).unwrap(),
                JsValue::String("set".to_owned())
            );
        }

        {
            let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
                .expect("engine init");
            let echo_url = format!("{base_url}/echo");
            let expr = format!(
                "fetch(new Request({echo_url:?}, {{ credentials: 'omit' }})).then((response) => response.text())"
            );
            assert_eq!(rt.evaluate(&expr).unwrap(), JsValue::String(String::new()));
        }

        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fetch_credentials_omit_does_not_store_response_cookies() {
        let (base_url, network_config, server) =
            spawn_cookie_echo_server("vixen-cookie-credentials-store-omit.com");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-cookie-credentials-store-omit-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);

        let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
            .expect("engine init");
        let set_url = format!("{base_url}/set");
        let expr = format!(
            "fetch({set_url:?}, {{ credentials: 'omit' }}).then((response) => response.text())"
        );
        assert_eq!(
            rt.evaluate(&expr).unwrap(),
            JsValue::String("set".to_owned())
        );

        let echo_url = format!("{base_url}/echo");
        let expr = format!("fetch({echo_url:?}).then((response) => response.text())");
        assert_eq!(rt.evaluate(&expr).unwrap(), JsValue::String(String::new()));

        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fetch_get_writes_profile_cache_entry() {
        let (url, network_config, server) =
            spawn_fetch_server("vixen-fetch-cache.com", "cached body");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-fetch-cache-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);

        {
            let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
                .expect("engine init");
            let expr = format!("fetch({url:?}).then((response) => response.text())");
            assert_eq!(
                rt.evaluate(&expr).unwrap(),
                JsValue::String("cached body".to_owned())
            );
        }

        let parsed = url::Url::parse(&url).unwrap();
        let origin_key = vixen_net::Origin::from_url(&parsed).partition_key();
        let store = vixen_store::Store::open(&path).unwrap();
        let entry = store.get_cache(&origin_key, &url).unwrap().unwrap();
        assert_eq!(entry.status, 200);
        assert_eq!(entry.body, b"cached body");
        assert!(
            entry
                .headers
                .iter()
                .any(|(name, value)| name == "x-vixen-test" && value == "yes")
        );

        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fetch_no_cache_revalidates_cached_response() {
        let (url, network_config, server) = spawn_revalidation_server("vixen-fetch-revalidate.com");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-fetch-revalidate-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);

        {
            let mut rt =
                JsRuntime::with_network_config_and_storage_path(network_config.clone(), &path)
                    .expect("engine init");
            let expr = format!("fetch({url:?}).then((response) => response.text())");
            assert_eq!(
                rt.evaluate(&expr).unwrap(),
                JsValue::String("cached-v1".to_owned())
            );

            let expr = format!(
                "fetch({url:?}, {{ cache: 'no-cache' }}).then((response) => response.text().then((body) => response.status + ':' + response.headers.get('etag') + ':' + body), (err) => 'ERR:' + err.message)"
            );
            assert_eq!(
                rt.evaluate(&expr).unwrap(),
                JsValue::String("200:\"v1\":cached-v1".to_owned())
            );
        }

        let parsed = url::Url::parse(&url).unwrap();
        let origin_key = vixen_net::Origin::from_url(&parsed).partition_key();
        let store = vixen_store::Store::open(&path).unwrap();
        let entry = store.get_cache(&origin_key, &url).unwrap().unwrap();
        assert_eq!(entry.status, 200);
        assert_eq!(entry.body, b"cached-v1");

        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fetch_expires_freshness_and_request_no_cache_share_profile_decision() {
        use std::sync::atomic::Ordering;

        let (url, network_config, requests, server) =
            spawn_expires_revalidation_server("vixen-fetch-expires.com");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-fetch-expires-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
            .expect("engine init");
        let default_fetch = format!("fetch({url:?}).then((response) => response.text())");

        assert_eq!(
            rt.evaluate(&default_fetch).unwrap(),
            JsValue::String("expires-v1".to_owned())
        );
        assert_eq!(
            rt.evaluate(&default_fetch).unwrap(),
            JsValue::String("expires-v1".to_owned())
        );
        let revalidate = format!(
            "fetch({url:?}, {{ headers: {{ 'Cache-Control': 'no-cache' }} }}).then((response) => response.text())"
        );
        assert_eq!(
            rt.evaluate(&revalidate).unwrap(),
            JsValue::String("expires-v1".to_owned())
        );

        server.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        drop(rt);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fetch_default_reuses_fresh_vary_match_without_network() {
        use std::sync::atomic::Ordering;

        let (url, network_config, requests, server) =
            spawn_fresh_vary_server("vixen-fetch-fresh-vary.com");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-fetch-fresh-vary-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
            .expect("engine init");
        let expression = format!(
            "fetch({url:?}, {{ headers: {{ 'Accept-Language': 'en' }} }}).then((response) => response.text())"
        );

        assert_eq!(
            rt.evaluate(&expression).unwrap(),
            JsValue::String("response-1-en".to_owned())
        );
        assert_eq!(
            rt.evaluate(&expression).unwrap(),
            JsValue::String("response-1-en".to_owned())
        );
        server.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        drop(rt);

        let parsed = url::Url::parse(&url).unwrap();
        let origin_key = vixen_net::Origin::from_url(&parsed).partition_key();
        let entry = vixen_store::Store::open(&path)
            .unwrap()
            .get_cache(&origin_key, &url)
            .unwrap()
            .unwrap();
        assert_eq!(
            entry.vary_headers,
            vec![("accept-language".to_owned(), Some("en".to_owned()))]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fetch_default_refetches_when_vary_value_changes() {
        use std::sync::atomic::Ordering;

        let (url, network_config, requests, server) =
            spawn_fresh_vary_server("vixen-fetch-vary-change.com");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-fetch-vary-change-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
            .expect("engine init");
        for (language, expected) in [
            ("en", "response-1-en"),
            ("fr", "response-2-fr"),
            ("en", "response-1-en"),
        ] {
            let expression = format!(
                "fetch({url:?}, {{ headers: {{ 'Accept-Language': {language:?} }} }}).then((response) => response.text())"
            );
            assert_eq!(
                rt.evaluate(&expression).unwrap(),
                JsValue::String(expected.to_owned())
            );
        }
        server.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fetch_no_store_skips_profile_cache_entry() {
        let (url, network_config, server) =
            spawn_fetch_server("vixen-fetch-no-store-cache.com", "uncached body");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-fetch-no-store-cache-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);

        {
            let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
                .expect("engine init");
            let expr = format!(
                "fetch({url:?}, {{ cache: 'no-store' }}).then((response) => response.text())"
            );
            assert_eq!(
                rt.evaluate(&expr).unwrap(),
                JsValue::String("uncached body".to_owned())
            );
        }

        let parsed = url::Url::parse(&url).unwrap();
        let origin_key = vixen_net::Origin::from_url(&parsed).partition_key();
        let store = vixen_store::Store::open(&path).unwrap();
        assert!(store.get_cache(&origin_key, &url).unwrap().is_none());

        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fetch_request_no_store_directive_skips_profile_cache_entry() {
        let (url, network_config, server) =
            spawn_fetch_server("vixen-fetch-request-no-store.com", "uncached body");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-fetch-request-no-store-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);

        {
            let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
                .expect("engine init");
            let expression = format!(
                "fetch({url:?}, {{ headers: {{ 'Cache-Control': 'no-store' }} }}).then((response) => response.text())"
            );
            assert_eq!(
                rt.evaluate(&expression).unwrap(),
                JsValue::String("uncached body".to_owned())
            );
        }

        let parsed = url::Url::parse(&url).unwrap();
        let origin_key = vixen_net::Origin::from_url(&parsed).partition_key();
        let store = vixen_store::Store::open(&path).unwrap();
        assert!(store.cache_variants(&origin_key, &url).unwrap().is_empty());

        server.join().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fetch_force_cache_reads_profile_cache_without_network() {
        let (url, network_config, server) =
            spawn_fetch_server("vixen-fetch-force-cache.com", "cached body");
        let path = std::env::temp_dir().join(format!(
            "vixen-engine-fetch-force-cache-test-{}-{}.redb",
            std::process::id(),
            STORAGE_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);

        {
            let mut rt =
                JsRuntime::with_network_config_and_storage_path(network_config.clone(), &path)
                    .expect("engine init");
            let expr = format!("fetch({url:?}).then((response) => response.text())");
            assert_eq!(
                rt.evaluate(&expr).unwrap(),
                JsValue::String("cached body".to_owned())
            );
        }

        server.join().unwrap();

        {
            let mut rt = JsRuntime::with_network_config_and_storage_path(network_config, &path)
                .expect("engine init");
            let expr = format!(
                "fetch(new Request({url:?}, {{ cache: 'force-cache' }})).then((response) => response.text().then((body) => response.status + ':' + response.url + ':' + response.headers.get('x-vixen-test') + ':' + body))"
            );
            assert_eq!(
                rt.evaluate(&expr).unwrap(),
                JsValue::String(format!("200:{url}:yes:cached body"))
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fetch_only_if_cached_rejects_profile_cache_miss() {
        let mut rt = JsRuntime::new().expect("engine init");

        assert_eq!(
            rt.evaluate("fetch('http://vixen-cache-miss.com/payload', { cache: 'only-if-cached' }).then(() => false, (err) => err instanceof TypeError && /fetch cache miss/.test(err.message))")
                .unwrap(),
            JsValue::Bool(true)
        );
    }

    #[test]
    fn fetch_rejects_invalid_cache_mode() {
        let mut rt = JsRuntime::new().expect("engine init");

        assert_eq!(
            rt.evaluate("fetch('http://vixen-invalid-cache-mode.com/payload', { cache: 'stale-magic' }).then(() => false, (err) => err instanceof TypeError && /unsupported fetch cache mode: stale-magic/.test(err.message))")
                .unwrap(),
            JsValue::Bool(true)
        );
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

    #[test]
    fn fetch_honors_page_connect_src_csp() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html(
            "https://page.test/index.html",
            "<meta http-equiv='Content-Security-Policy' content=\"connect-src 'self'\"><main></main>",
        )
        .unwrap();

        assert_eq!(
            rt.evaluate_with_page_mut(
                "fetch('https://example.org/api').then(() => false, (err) => err instanceof TypeError && /CSP connect-src/.test(err.message))",
                &mut page,
            )
            .unwrap(),
            JsValue::Bool(true)
        );
    }

    #[test]
    fn fetch_rechecks_redirect_target_policy_before_transport() {
        use std::sync::atomic::Ordering;

        let (url, network_config, requests, server) = spawn_cross_host_redirect_server(
            "vixen-fetch-csp-redirect.com",
            "vixen-fetch-csp-blocked.com",
        );
        let initial_origin = url.strip_suffix("/redirect").unwrap();
        let html = format!(
            "<meta http-equiv='Content-Security-Policy' content=\"connect-src {initial_origin}\"><main></main>"
        );
        let mut page = Page::from_html("http://source.test/page", &html).unwrap();
        let mut rt = JsRuntime::with_network_config(network_config).expect("engine init");
        let expression = format!(
            "fetch({url:?}).then(() => false, (error) => /CSP connect-src/.test(error.message))"
        );

        assert_eq!(
            rt.evaluate_with_page_mut(&expression, &mut page).unwrap(),
            JsValue::Bool(true)
        );
        server.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fetch_blocks_active_mixed_content_from_secure_pages() {
        let mut rt = JsRuntime::new().expect("engine init");
        let mut page = Page::from_html("https://secure.test/index.html", "<main></main>").unwrap();

        assert_eq!(
            rt.evaluate_with_page_mut(
                "fetch('http://example.org/api').then(() => false, (err) => err instanceof TypeError && /active mixed content/.test(err.message))",
                &mut page,
            )
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
