//! Static page-module dependency loading through BrowserCore's shared resource boundary.
//!
//! V8 owns graph discovery and evaluation. This loader resolves static file and
//! HTTP(S) imports, then delegates bytes, redirects, CORS, CSP/mixed-content
//! checks, profile credentials/cache writes, and bounded network diagnostics to
//! the same external-resource loader used by parser scripts, stylesheets, and
//! images. Dynamic imports, import attributes, and graphs over the explicit
//! load/event limits fail closed before module evaluation.

use std::collections::{HashMap, VecDeque};
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
const MAX_MODULE_NETWORK_EVENTS: usize = 1024;

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

#[derive(Default)]
struct LoaderState {
    request: Option<ExternalPageScript>,
    graph_loads: usize,
    events: VecDeque<JsNetworkEvent>,
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
        state.request = Some(request);
        state.graph_loads = 0;
        Ok(())
    }

    pub(super) fn drain_events(&self) -> Result<Vec<JsNetworkEvent>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "module loader state is poisoned".to_owned())?;
        Ok(state.events.drain(..).collect())
    }

    pub(super) fn shutdown(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.request = None;
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
    ) -> Result<(String, ExternalPageScript), (Option<String>, &'static str, String)> {
        let request_id = self
            .request_ids
            .next()
            .map_err(|message| (None, "module-policy", message))?;
        let mut state = self.state.lock().map_err(|_| {
            (
                Some(request_id.clone()),
                "module-policy",
                "module loader state is poisoned".to_owned(),
            )
        })?;
        if state.graph_loads >= MAX_MODULE_GRAPH_LOADS {
            return Err((
                Some(request_id),
                "module-policy",
                format!("module graph exceeds {MAX_MODULE_GRAPH_LOADS} dependency loads"),
            ));
        }
        state.graph_loads += 1;
        let root = state.request.clone().ok_or_else(|| {
            (
                Some(request_id.clone()),
                "module-policy",
                "module dependency load has no active graph policy".to_owned(),
            )
        })?;
        drop(state);
        let request = root
            .module_dependency(module_specifier.clone())
            .map_err(|reason| {
                (
                    Some(request_id.clone()),
                    reason,
                    format!("module dependency blocked by {reason}: {module_specifier}"),
                )
            })?;
        Ok((request_id, request))
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

    fn resolve_module_specifier(
        &self,
        specifier: &str,
        referrer: &str,
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
        let import_map = self
            .state
            .lock()
            .map_err(|_| ModuleLoaderError::generic("module loader state is poisoned"))?
            .request
            .as_ref()
            .and_then(ExternalPageScript::import_map);
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
        if kind == ResolutionKind::DynamicImport {
            return Err(ModuleLoaderError::generic(
                "dynamic import is not enabled for page module graphs",
            ));
        }
        self.resolve_module_specifier(specifier, referrer)
    }

    fn resolve_with_scope(
        &self,
        _scope: &mut deno_core::v8::PinScope,
        specifier: &str,
        referrer: &str,
        kind: ResolutionKind,
        import_attributes: &HashMap<String, String>,
    ) -> ModuleResolveResponse {
        if !import_attributes.is_empty() {
            return Err(ModuleLoaderError::generic(
                "import attributes are not enabled for page module graphs",
            ));
        }
        self.resolve(specifier, referrer, kind)
    }

    fn import_meta_resolve(
        &self,
        specifier: &str,
        referrer: &str,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        self.resolve_module_specifier(specifier, referrer)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        if options.is_dynamic_import {
            return ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(
                "dynamic import is not enabled for page module graphs",
            )));
        }
        if options.requested_module_type != RequestedModuleType::None {
            return ModuleLoadResponse::Sync(Err(ModuleLoaderError::generic(
                "import attributes are not enabled for page module graphs",
            )));
        }

        let module_specifier = module_specifier.clone();
        let (request_id, request) = match self.prepare_request(&module_specifier) {
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
        ModuleLoadResponse::Async(Box::pin(AbortModuleLoad {
            task: executor.spawn(async move {
                loader
                    .load_dependency(request_id, request, module_specifier)
                    .await
            }),
        }))
    }
}

struct AbortModuleLoad {
    task: tokio::task::JoinHandle<Result<ModuleSource, String>>,
}

impl Future for AbortModuleLoad {
    type Output = Result<ModuleSource, ModuleLoaderError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.task).poll(context) {
            Poll::Ready(Ok(result)) => Poll::Ready(result.map_err(ModuleLoaderError::generic)),
            Poll::Ready(Err(error)) => Poll::Ready(Err(ModuleLoaderError::generic(format!(
                "module loader task failed: {error}"
            )))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for AbortModuleLoad {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PageModuleLoader {
    async fn load_dependency(
        &self,
        request_id: String,
        request: ExternalPageScript,
        specified_url: Url,
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
                request,
                revalidate_profile_cache: profile_cache_enabled,
                max_body_bytes: self.network_config.max_body_bytes,
                max_redirects: self.network_config.max_redirects,
            },
        )
        .await;
        let cookie_delta = cookies.delta_from_snapshots(&profile_baseline);
        let mut events = module_network_events(&request_id, specified_url.as_str(), &result);

        let (found_url, body, requested_urls, cache_response) = match result {
            Ok(LoadedExternalResource::File { final_url, body }) => {
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
                if !module_script_response_allowed(&response) {
                    let message = format!(
                        "module dependency response policy rejected {}",
                        response.final_url
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

        if let Err(error) = persist_profile_cookies(&store, &requested_urls, &cookie_delta) {
            return self.record_load_failure(
                events,
                request_id,
                found_url.to_string(),
                "profile",
                error.to_string(),
            );
        }
        if let Err(message) = self.network_state.apply_cookie_delta(cookie_delta) {
            return self.record_load_failure(
                events,
                request_id,
                found_url.to_string(),
                "profile",
                message,
            );
        }
        if profile_cache_enabled
            && let Some(response) = cache_response.as_ref()
            && let Err(error) = persist_external_resource_cache(&store, response)
        {
            return self.record_load_failure(
                events,
                request_id,
                found_url.to_string(),
                "profile",
                error.to_string(),
            );
        }
        self.record_events(events)?;

        Ok(ModuleSource::new_with_redirect(
            ModuleType::JavaScript,
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
    }
}
