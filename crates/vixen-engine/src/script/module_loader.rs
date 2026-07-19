//! Page-module graph loading through BrowserCore's shared resource boundary.
//!
//! V8 owns graph discovery and evaluation. This loader resolves static file and
//! HTTP(S) imports, then delegates bytes, redirects, CORS, CSP/mixed-content
//! checks, profile credentials/cache writes, and bounded network diagnostics to
//! the same external-resource loader used by parser scripts, stylesheets, and
//! images. Static and dynamic imports retain their originating graph policy;
//! exact JSON import attributes use destination-specific response policy, while
//! unsupported attributes and graphs over explicit limits fail closed before
//! module evaluation.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use deno_core::error::ModuleLoaderError;
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleResolveResponse,
    ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, RequestedModuleType,
    ResolutionKind,
};
use url::Url;
use vixen_net::{CookieJar, Method, Network, NetworkEvent};

use super::import_maps::MAX_MODULE_SPECIFIER_BYTES;
use super::webapi::{CacheDisabledFlag, RuntimeNetworkState, WebStorageBackend};
use super::{ExternalPageScript, JsNetworkEvent, persist_profile_cookies};
use crate::browser::{
    ExternalResourceLoadInput, LoadedExternalResource, SharedRequestIdAllocator,
    load_external_resource, module_script_response_allowed, persist_external_resource_cache,
};

const MAX_MODULE_GRAPH_LOADS: usize = 64;
const MAX_MODULE_GRAPH_URLS: usize = 1 + MAX_MODULE_GRAPH_LOADS * 2;
const MAX_MODULE_NETWORK_EVENTS: usize = 1024;
const MAX_MODULE_PROVENANCE: usize = 1024;

type PreparedModuleRequest = (String, ExternalPageScript, u64);
type ModuleRequestError = (Option<String>, &'static str, String);

fn validate_import_attributes(
    import_attributes: &HashMap<String, String>,
) -> Result<(), ModuleLoaderError> {
    if import_attributes_supported(import_attributes) {
        return Ok(());
    }
    Err(ModuleLoaderError::generic(
        "unsupported module import attributes; only type=json is enabled",
    ))
}

fn import_attributes_supported(import_attributes: &HashMap<String, String>) -> bool {
    import_attributes.is_empty()
        || import_attributes.len() == 1
            && import_attributes
                .get("type")
                .is_some_and(|value| value == "json")
}

#[derive(Clone)]
enum RequestIds {
    Browser(SharedRequestIdAllocator),
    Local(Arc<AtomicU64>),
}

impl RequestIds {
    fn next(&self) -> Result<String, String> {
        match self {
            Self::Browser(ids) => ids.next_string(),
            Self::Local(next) => next
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .map(|value| value.to_string())
                .map_err(|_| "module request id space is exhausted".to_owned()),
        }
    }
}

struct ModuleGraphContext {
    request: ExternalPageScript,
    loads: usize,
    module_urls: usize,
}

#[derive(Default)]
struct LoaderState {
    modules: BTreeMap<String, Arc<Mutex<ModuleGraphContext>>>,
    denied_attribute_imports: BTreeSet<(String, String)>,
    attribute_denial_overflow: bool,
    events: VecDeque<JsNetworkEvent>,
    active_tasks: BTreeMap<u64, tokio::task::AbortHandle>,
    next_task_id: u64,
    generation: u64,
}

#[derive(Clone)]
pub(super) struct PageModuleLoader {
    network_config: vixen_net::NetworkConfig,
    storage: Arc<Mutex<WebStorageBackend>>,
    network_state: RuntimeNetworkState,
    cache_disabled: CacheDisabledFlag,
    request_ids: RequestIds,
    executor: Arc<Mutex<Option<Arc<tokio::runtime::Runtime>>>>,
    state: Arc<Mutex<LoaderState>>,
}

impl PageModuleLoader {
    pub(super) fn new(
        network_config: vixen_net::NetworkConfig,
        storage: WebStorageBackend,
        network_state: RuntimeNetworkState,
        cache_disabled: CacheDisabledFlag,
        request_ids: Option<SharedRequestIdAllocator>,
        executor: Option<Arc<tokio::runtime::Runtime>>,
    ) -> Self {
        Self {
            network_config,
            storage: Arc::new(Mutex::new(storage)),
            network_state,
            cache_disabled,
            request_ids: request_ids.map_or_else(
                || RequestIds::Local(Arc::new(AtomicU64::new(1))),
                RequestIds::Browser,
            ),
            executor: Arc::new(Mutex::new(executor)),
            state: Arc::new(Mutex::new(LoaderState::default())),
        }
    }

    pub(super) fn begin_graph(&self, request: ExternalPageScript) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "module loader state is poisoned".to_owned())?;
        let root_url = request.url().to_string();
        if state.modules.contains_key(&root_url) {
            return Ok(());
        }
        if state.modules.len() >= MAX_MODULE_PROVENANCE {
            return Err(format!(
                "module provenance exceeds {MAX_MODULE_PROVENANCE} URLs"
            ));
        }
        state.modules.insert(
            root_url,
            Arc::new(Mutex::new(ModuleGraphContext {
                request,
                loads: 0,
                module_urls: 1,
            })),
        );
        Ok(())
    }

    pub(super) fn document_import_map(&self) -> Option<super::import_maps::PageImportMap> {
        let state = self.state.lock().ok()?;
        state.modules.values().find_map(|graph| {
            graph
                .lock()
                .ok()
                .and_then(|graph| graph.request.import_map())
        })
    }

    pub(super) fn has_pending_loads(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| !state.active_tasks.is_empty())
    }

    pub(super) fn cancel_pending_loads(&self) {
        let handles = if let Ok(mut state) = self.state.lock() {
            state.generation = state.generation.saturating_add(1);
            std::mem::take(&mut state.active_tasks)
                .into_values()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for handle in handles {
            handle.abort();
        }
    }

    pub(super) fn reset_realm(&self) {
        self.cancel_pending_loads();
        if let Ok(mut state) = self.state.lock() {
            state.modules.clear();
            state.denied_attribute_imports.clear();
            state.attribute_denial_overflow = false;
        }
    }

    pub(super) fn validate_import_attributes_in_scope(
        &self,
        scope: &mut deno_core::v8::PinScope,
        import_attributes: &HashMap<String, String>,
        context: &deno_core::ImportAttributesContext,
    ) {
        if import_attributes_supported(import_attributes) {
            return;
        }
        if context.line_number.is_none()
            && let Ok(mut state) = self.state.lock()
        {
            if state.denied_attribute_imports.len() < MAX_MODULE_PROVENANCE {
                state
                    .denied_attribute_imports
                    .insert((context.referrer.clone(), context.specifier.clone()));
            } else {
                state.attribute_denial_overflow = true;
            }
        }
        let message = format!(
            "unsupported module import attributes; only type=json is enabled{}",
            context.format_location()
        );
        let Some(message) = deno_core::v8::String::new(scope, &message) else {
            return;
        };
        let exception = deno_core::v8::Exception::type_error(scope, message);
        scope.throw_exception(exception);
    }

    fn take_denied_attribute_import(
        &self,
        referrer: &str,
        specifier: &str,
    ) -> Result<bool, ModuleLoaderError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ModuleLoaderError::generic("module loader state is poisoned"))?;
        Ok(state.attribute_denial_overflow
            || state
                .denied_attribute_imports
                .remove(&(referrer.to_owned(), specifier.to_owned())))
    }

    pub(super) fn drain_events(&self) -> Result<Vec<JsNetworkEvent>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "module loader state is poisoned".to_owned())?;
        Ok(state.events.drain(..).collect())
    }

    pub(super) fn shutdown(&self) {
        self.reset_realm();
        if let Ok(mut state) = self.state.lock() {
            state.events.clear();
        }
        if let Ok(mut storage) = self.storage.lock() {
            *storage = WebStorageBackend::memory();
        }
        if let Ok(mut executor) = self.executor.lock() {
            executor.take();
        }
    }

    fn prepare_request(
        &self,
        module_specifier: &ModuleSpecifier,
    ) -> Result<PreparedModuleRequest, ModuleRequestError> {
        let request_id = self
            .request_ids
            .next()
            .map_err(|message| (None, "module-policy", message))?;
        let state = self.state.lock().map_err(|_| {
            (
                Some(request_id.clone()),
                "module-policy",
                "module loader state is poisoned".to_owned(),
            )
        })?;
        let graph = state
            .modules
            .get(module_specifier.as_str())
            .cloned()
            .ok_or_else(|| {
                (
                    Some(request_id.clone()),
                    "module-policy",
                    "module dependency load has no graph provenance".to_owned(),
                )
            })?;
        let generation = state.generation;
        drop(state);
        let mut graph = graph.lock().map_err(|_| {
            (
                Some(request_id.clone()),
                "module-policy",
                "module graph provenance is poisoned".to_owned(),
            )
        })?;
        if graph.loads >= MAX_MODULE_GRAPH_LOADS {
            return Err((
                Some(request_id),
                "module-policy",
                format!("module graph exceeds {MAX_MODULE_GRAPH_LOADS} dependency loads"),
            ));
        }
        graph.loads += 1;
        let request = graph
            .request
            .module_dependency(module_specifier.clone())
            .map_err(|reason| {
                (
                    Some(request_id.clone()),
                    reason,
                    format!("module dependency blocked by {reason}: {module_specifier}"),
                )
            })?;
        Ok((request_id, request, generation))
    }

    fn record_events(&self, events: Vec<JsNetworkEvent>) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "module loader state is poisoned".to_owned())?;
        if state.events.len().saturating_add(events.len()) > MAX_MODULE_NETWORK_EVENTS {
            return Err(format!(
                "module network diagnostics exceed {MAX_MODULE_NETWORK_EVENTS} events"
            ));
        }
        state.events.extend(events);
        Ok(())
    }

    fn record_blocked(
        &self,
        request_id: String,
        url: String,
        reason: &'static str,
        message: String,
    ) -> Result<(), String> {
        self.record_events(vec![
            JsNetworkEvent::Request {
                request_id: request_id.clone(),
                url: url.clone(),
                method: Method::Get.as_str().to_owned(),
            },
            JsNetworkEvent::Failure {
                request_id,
                url,
                error_text: message,
                blocked_reason: Some(reason.to_owned()),
            },
        ])
    }

    fn executor(&self) -> Result<Arc<tokio::runtime::Runtime>, String> {
        let mut executor = self
            .executor
            .lock()
            .map_err(|_| "module loader executor is poisoned".to_owned())?;
        if let Some(runtime) = executor.as_ref() {
            return Ok(Arc::clone(runtime));
        }
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("vixen-module-loader")
                .enable_all()
                .build()
                .map_err(|error| {
                    format!("module loader executor initialisation failed: {error}")
                })?,
        );
        *executor = Some(Arc::clone(&runtime));
        Ok(runtime)
    }

    fn register_task(&self, handle: tokio::task::AbortHandle) -> Result<u64, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "module loader state is poisoned".to_owned())?;
        let task_id = state.next_task_id;
        state.next_task_id = state
            .next_task_id
            .checked_add(1)
            .ok_or_else(|| "module loader task id space is exhausted".to_owned())?;
        state.active_tasks.insert(task_id, handle);
        Ok(task_id)
    }

    fn unregister_task(&self, task_id: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.active_tasks.remove(&task_id);
        }
    }

    fn register_module_url(
        &self,
        graph: Arc<Mutex<ModuleGraphContext>>,
        url: &Url,
    ) -> Result<(), ModuleLoaderError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ModuleLoaderError::generic("module loader state is poisoned"))?;
        if state.modules.contains_key(url.as_str()) {
            return Ok(());
        }
        if state.modules.len() >= MAX_MODULE_PROVENANCE {
            return Err(ModuleLoaderError::generic(format!(
                "module provenance exceeds {MAX_MODULE_PROVENANCE} URLs"
            )));
        }
        let mut context = graph
            .lock()
            .map_err(|_| ModuleLoaderError::generic("module graph provenance is poisoned"))?;
        if context.module_urls >= MAX_MODULE_GRAPH_URLS {
            return Err(ModuleLoaderError::generic(format!(
                "module graph exceeds {MAX_MODULE_GRAPH_URLS} provenance URLs"
            )));
        }
        context.module_urls += 1;
        drop(context);
        state.modules.insert(url.to_string(), graph);
        Ok(())
    }

    fn register_redirect_url(&self, specified: &Url, found: &Url) -> Result<(), String> {
        if specified == found {
            return Ok(());
        }
        let graph = self
            .state
            .lock()
            .map_err(|_| "module loader state is poisoned".to_owned())?
            .modules
            .get(specified.as_str())
            .cloned()
            .ok_or_else(|| "redirected module has no graph provenance".to_owned())?;
        self.register_module_url(graph, found)
            .map_err(|error| error.to_string())
    }

    fn generation_is_active(&self, generation: u64) -> Result<bool, String> {
        self.state
            .lock()
            .map(|state| state.generation == generation)
            .map_err(|_| "module loader state is poisoned".to_owned())
    }

    fn resolve_module_specifier(
        &self,
        specifier: &str,
        referrer: &str,
        register: bool,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        if specifier.len() > MAX_MODULE_SPECIFIER_BYTES
            || referrer.len() > MAX_MODULE_SPECIFIER_BYTES
        {
            return Err(ModuleLoaderError::generic(
                "module specifier or referrer exceeds the loader limit",
            ));
        }
        if referrer == "." {
            let root = Url::parse(specifier).map_err(ModuleLoaderError::from_err)?;
            if root.as_str().len() > MAX_MODULE_SPECIFIER_BYTES {
                return Err(ModuleLoaderError::generic(
                    "module root URL exceeds the loader limit",
                ));
            }
            return Ok(root);
        }
        let referrer_url = Url::parse(referrer).map_err(ModuleLoaderError::from_err)?;
        let graph = self
            .state
            .lock()
            .map_err(|_| ModuleLoaderError::generic("module loader state is poisoned"))?
            .modules
            .get(referrer_url.as_str())
            .cloned()
            .ok_or_else(|| ModuleLoaderError::generic("module referrer has no graph provenance"))?;
        let import_map = graph
            .lock()
            .map_err(|_| ModuleLoaderError::generic("module graph provenance is poisoned"))?
            .request
            .import_map();
        let resolved = if let Some(import_map) = import_map {
            import_map
                .resolve(specifier, &referrer_url)
                .map_err(ModuleLoaderError::generic)?
        } else {
            deno_core::resolve_import(specifier, referrer).map_err(ModuleLoaderError::from_err)?
        };
        if resolved.as_str().len() > MAX_MODULE_SPECIFIER_BYTES {
            return Err(ModuleLoaderError::generic(
                "resolved module URL exceeds the loader limit",
            ));
        }
        if register {
            self.register_module_url(graph, &resolved)?;
        }
        Ok(resolved)
    }
}

impl ModuleLoader for PageModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: ResolutionKind,
    ) -> ModuleResolveResponse {
        if kind == ResolutionKind::DynamicImport
            && self.take_denied_attribute_import(referrer, specifier)?
        {
            return Err(ModuleLoaderError::generic(
                "unsupported module import attributes; only type=json is enabled",
            ));
        }
        self.resolve_module_specifier(specifier, referrer, true)
    }

    fn resolve_with_scope(
        &self,
        _scope: &mut deno_core::v8::PinScope,
        specifier: &str,
        referrer: &str,
        kind: ResolutionKind,
        import_attributes: &HashMap<String, String>,
    ) -> ModuleResolveResponse {
        validate_import_attributes(import_attributes)?;
        self.resolve(specifier, referrer, kind)
    }

    fn import_meta_resolve(
        &self,
        specifier: &str,
        referrer: &str,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        self.resolve_module_specifier(specifier, referrer, false)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        if !matches!(
            options.requested_module_type,
            RequestedModuleType::None | RequestedModuleType::Json
        ) {
            return ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(
                "unsupported module import attributes; only type=json is enabled",
            )));
        }

        let module_specifier = module_specifier.clone();
        let requested_module_type = options.requested_module_type;
        let (request_id, request, generation) = match self.prepare_request(&module_specifier) {
            Ok(value) => value,
            Err((request_id, reason, message)) => {
                if let Some(request_id) = request_id {
                    let _ = self.record_blocked(
                        request_id,
                        module_specifier.to_string(),
                        reason,
                        message.clone(),
                    );
                }
                return ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(message)));
            }
        };
        let executor = match self.executor() {
            Ok(executor) => executor,
            Err(message) => {
                let _ = self.record_blocked(
                    request_id,
                    module_specifier.to_string(),
                    "load",
                    message.clone(),
                );
                return ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(message)));
            }
        };
        let loader = self.clone();
        let task = executor.spawn(async move {
            loader
                .load_dependency(
                    request_id,
                    request,
                    module_specifier,
                    requested_module_type,
                    generation,
                )
                .await
        });
        let task_id = match self.register_task(task.abort_handle()) {
            Ok(task_id) => task_id,
            Err(message) => {
                task.abort();
                return ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(message)));
            }
        };
        ModuleLoadResponse::Async(Box::pin(AbortModuleLoad {
            task,
            loader: self.clone(),
            task_id,
        }))
    }
}

struct AbortModuleLoad {
    task: tokio::task::JoinHandle<Result<ModuleSource, String>>,
    loader: PageModuleLoader,
    task_id: u64,
}

impl Future for AbortModuleLoad {
    type Output = Result<ModuleSource, ModuleLoaderError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.task).poll(context) {
            Poll::Ready(Ok(result)) => {
                self.loader.unregister_task(self.task_id);
                Poll::Ready(result.map_err(ModuleLoaderError::generic))
            }
            Poll::Ready(Err(error)) => {
                self.loader.unregister_task(self.task_id);
                Poll::Ready(Err(ModuleLoaderError::generic(format!(
                    "module loader task failed: {error}"
                ))))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for AbortModuleLoad {
    fn drop(&mut self) {
        self.task.abort();
        self.loader.unregister_task(self.task_id);
    }
}

impl PageModuleLoader {
    async fn load_dependency(
        &self,
        request_id: String,
        request: ExternalPageScript,
        specified_url: Url,
        requested_module_type: RequestedModuleType,
        generation: u64,
    ) -> Result<ModuleSource, String> {
        let store = match self
            .storage
            .lock()
            .map_err(|_| "module loader storage is poisoned".to_owned())?
            .store()
        {
            Some(store) => store,
            None => {
                let message = "module dependency loader requires profile storage".to_owned();
                self.record_blocked(
                    request_id,
                    specified_url.to_string(),
                    "profile",
                    message.clone(),
                )?;
                return Err(message);
            }
        };
        let mut profile_baseline = self.network_state.cookie_snapshots()?;
        let profile_cache_enabled = !self.cache_disabled.snapshot();
        let mut cookies = CookieJar::from_snapshots(profile_baseline.clone());
        let mut network = match Network::new(self.network_config.clone()) {
            Ok(network) => network,
            Err(error) => {
                let message = format!("module dependency network initialisation failed: {error}");
                self.record_blocked(
                    request_id,
                    specified_url.to_string(),
                    "load",
                    message.clone(),
                )?;
                return Err(message);
            }
        };
        let result = load_external_resource(
            &mut network,
            &mut cookies,
            ExternalResourceLoadInput {
                store: &store,
                profile_baseline: &mut profile_baseline,
                request: request.clone(),
                revalidate_profile_cache: profile_cache_enabled,
                max_body_bytes: self.network_config.max_body_bytes,
                max_redirects: self.network_config.max_redirects,
            },
        )
        .await;
        if !self.generation_is_active(generation)? {
            return Err("module dependency load was cancelled".to_owned());
        }
        let cookie_delta = cookies.delta_from_snapshots(&profile_baseline);
        let mut events = module_network_events(&request_id, specified_url.as_str(), &result);

        let (found_url, body, requested_urls, cache_response) = match result {
            Ok(LoadedExternalResource::File { final_url, body }) => {
                let is_json_file = final_url.path().to_ascii_lowercase().ends_with(".json");
                if requested_module_type == RequestedModuleType::Json && !is_json_file {
                    let message =
                        format!("JSON module file does not have a .json URL: {final_url}");
                    return self.record_load_failure(
                        events,
                        request_id,
                        final_url.to_string(),
                        "response-policy",
                        message,
                    );
                }
                if requested_module_type == RequestedModuleType::None && is_json_file {
                    let message =
                        format!("JSON module requires a type=json import attribute: {final_url}");
                    return self.record_load_failure(
                        events,
                        request_id,
                        final_url.to_string(),
                        "response-policy",
                        message,
                    );
                }
                (final_url, body, Vec::new(), None)
            }
            Ok(LoadedExternalResource::Http {
                response,
                requested_urls,
            }) => {
                let found_url = match Url::parse(&response.final_url) {
                    Ok(found_url) => found_url,
                    Err(error) => {
                        let message = format!("module response returned an invalid URL: {error}");
                        return self.record_load_failure(
                            events,
                            request_id,
                            response.final_url,
                            "response-policy",
                            message,
                        );
                    }
                };
                let response_allowed = match requested_module_type {
                    RequestedModuleType::None => module_script_response_allowed(&response),
                    RequestedModuleType::Json => json_module_response_allowed(&response),
                    _ => false,
                };
                if !response_allowed {
                    let message = format!(
                        "{} module response policy rejected {}",
                        if requested_module_type == RequestedModuleType::Json {
                            "JSON"
                        } else {
                            "JavaScript"
                        },
                        response.final_url,
                    );
                    events.push(JsNetworkEvent::Failure {
                        request_id: request_id.clone(),
                        url: response.final_url,
                        error_text: message.clone(),
                        blocked_reason: Some("response-policy".to_owned()),
                    });
                    self.record_events(events)?;
                    return Err(message);
                }
                (
                    found_url,
                    response.body.clone(),
                    requested_urls,
                    Some(response),
                )
            }
            Err(failure) => {
                let message = failure.error.to_string();
                self.record_events(events)?;
                return Err(message);
            }
        };

        if let Some(message) = request.integrity_failure(&body) {
            return self.record_load_failure(
                events,
                request_id,
                found_url.to_string(),
                "integrity",
                message,
            );
        }
        if let Err(message) = self.register_redirect_url(&specified_url, &found_url) {
            return self.record_load_failure(
                events,
                request_id,
                found_url.to_string(),
                "module-policy",
                message,
            );
        }
        let state = self
            .state
            .lock()
            .map_err(|_| "module loader state is poisoned".to_owned())?;
        if state.generation != generation {
            return Err("module dependency load was cancelled".to_owned());
        }
        let profile_result = persist_profile_cookies(&store, &requested_urls, &cookie_delta)
            .map_err(|error| error.to_string())
            .and_then(|()| self.network_state.apply_cookie_delta(cookie_delta))
            .and_then(|()| {
                if profile_cache_enabled && let Some(response) = cache_response.as_ref() {
                    persist_external_resource_cache(&store, response)
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            });
        drop(state);
        if let Err(message) = profile_result {
            return self.record_load_failure(
                events,
                request_id,
                found_url.to_string(),
                "profile",
                message,
            );
        }
        self.record_events(events)?;

        Ok(ModuleSource::new_with_redirect(
            if requested_module_type == RequestedModuleType::Json {
                ModuleType::Json
            } else {
                ModuleType::JavaScript
            },
            ModuleSourceCode::Bytes(body.into_boxed_slice().into()),
            &specified_url,
            &found_url,
            None,
        ))
    }

    fn record_load_failure<T>(
        &self,
        mut events: Vec<JsNetworkEvent>,
        request_id: String,
        url: String,
        blocked_reason: &'static str,
        message: String,
    ) -> Result<T, String> {
        events.push(JsNetworkEvent::Failure {
            request_id,
            url,
            error_text: message.clone(),
            blocked_reason: Some(blocked_reason.to_owned()),
        });
        self.record_events(events)?;
        Err(message)
    }
}

fn json_module_response_allowed(response: &vixen_net::ByteResponse) -> bool {
    if !(200..300).contains(&response.status) {
        return false;
    }
    response.content_type().is_some_and(is_json_module_mime)
}

fn is_json_module_mime(value: &str) -> bool {
    crate::mime::MimeType::parse(value).is_some_and(|mime| {
        mime.is_essence("application/json")
            || mime.subtype.len() > "+json".len() && mime.subtype.ends_with("+json")
    })
}

fn module_network_events(
    request_id: &str,
    request_url: &str,
    result: &Result<LoadedExternalResource, crate::browser::ExternalResourceLoadFailure>,
) -> Vec<JsNetworkEvent> {
    match result {
        Ok(LoadedExternalResource::File { final_url, .. }) => vec![
            JsNetworkEvent::Request {
                request_id: request_id.to_owned(),
                url: request_url.to_owned(),
                method: Method::Get.as_str().to_owned(),
            },
            JsNetworkEvent::Response {
                request_id: request_id.to_owned(),
                url: final_url.to_string(),
                status: 200,
            },
        ],
        Ok(LoadedExternalResource::Http { response, .. }) => response
            .events
            .iter()
            .map(|event| network_event(request_id, event))
            .collect(),
        Err(failure) => {
            let mut events: Vec<_> = failure
                .events
                .iter()
                .map(|event| network_event(request_id, event))
                .collect();
            if events.is_empty() {
                events.push(JsNetworkEvent::Request {
                    request_id: request_id.to_owned(),
                    url: request_url.to_owned(),
                    method: Method::Get.as_str().to_owned(),
                });
            }
            events.push(JsNetworkEvent::Failure {
                request_id: request_id.to_owned(),
                url: failure.url.clone(),
                error_text: failure.error.to_string(),
                blocked_reason: Some(failure.blocked_reason.to_owned()),
            });
            events
        }
    }
}

fn network_event(request_id: &str, event: &NetworkEvent) -> JsNetworkEvent {
    match event {
        NetworkEvent::RequestStart { url, method } => JsNetworkEvent::Request {
            request_id: request_id.to_owned(),
            url: url.clone(),
            method: method.as_str().to_owned(),
        },
        NetworkEvent::Redirect { from, to, status } => JsNetworkEvent::Redirect {
            request_id: request_id.to_owned(),
            from: from.clone(),
            to: to.clone(),
            status: *status,
        },
        NetworkEvent::Response { url, status } => JsNetworkEvent::Response {
            request_id: request_id.to_owned(),
            url: url.clone(),
            status: *status,
        },
        NetworkEvent::BodyProgress {
            url,
            chunk_bytes,
            loaded_bytes,
            total_bytes,
        } => JsNetworkEvent::Progress {
            request_id: request_id.to_owned(),
            url: url.clone(),
            chunk_bytes: *chunk_bytes,
            loaded_bytes: *loaded_bytes,
            total_bytes: *total_bytes,
        },
        NetworkEvent::Completed { url, body_bytes } => JsNetworkEvent::Completed {
            request_id: request_id.to_owned(),
            url: url.clone(),
            body_bytes: *body_bytes,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{is_json_module_mime, validate_import_attributes};
    use std::collections::HashMap;

    #[test]
    fn only_exact_json_import_attributes_are_supported() {
        assert!(validate_import_attributes(&HashMap::new()).is_ok());
        assert!(
            validate_import_attributes(&HashMap::from([("type".to_owned(), "json".to_owned())]))
                .is_ok()
        );
        assert!(
            validate_import_attributes(&HashMap::from([("type".to_owned(), "text".to_owned())]))
                .is_err()
        );
        assert!(
            validate_import_attributes(&HashMap::from([
                ("type".to_owned(), "json".to_owned()),
                ("unsupported".to_owned(), "yes".to_owned()),
            ]))
            .is_err()
        );
    }

    #[test]
    fn json_module_mime_is_strict() {
        assert!(is_json_module_mime("application/json"));
        assert!(is_json_module_mime("application/manifest+json"));
        assert!(!is_json_module_mime("text/javascript"));
        assert!(!is_json_module_mime("application/+json"));
        assert!(!is_json_module_mime("application/json/evil+json"));
    }
}
