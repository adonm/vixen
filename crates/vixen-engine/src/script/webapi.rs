//! JavaScript-only browser value-object compatibility layer.
//!
//! These bindings deliberately stay in `deno_core`/V8: they fill WebIDL-shaped
//! constructor/prototype behavior for pure value APIs before a backend is
//! involved. Page-backed DOM objects live in `script::dom`.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use deno_core::futures::future::{Either, select};
use deno_core::serde_json::{Value, json};
use deno_core::{Extension, ExtensionFileSource, OpState};
use url::Url;
use vixen_net::{
    ContentSecurityPolicy, CookieJar, CookieSnapshot, CorsCheckOutcome, CorsCredentialsMode,
    CorsResponseHeaders, IntegrityOutcome, Method, MixedContentVerdict, Network, NetworkConfig,
    NetworkEvent, Origin, RedirectMode, ResourceType, SameSite, TextRequest, TextResponse,
    classify_mixed_content, cors_check, cors_filtered_headers, parse_integrity,
    referrer_policy::{
        ReferrerPolicy, ReferrerValue, is_potentially_trustworthy, parse_referrer_policy,
        resolve_referrer,
    },
    validate_http_url, verify_integrity,
};
use vixen_store::{CookieRecord, PermissionDecision, Store};

use crate::doc::DocumentScriptItem;
use crate::headers::is_cors_safelisted_request_header;
use crate::page::Page;
use crate::storage_key::{
    MAX_PARTITION_BYTES, StorageKeyError, StorageKind, StorageQuota, validate_storage_key,
    validate_storage_value,
};

use super::RuntimeInterruptHandle;

struct WebApiHost {
    storage: WebStorageHost,
    network: Result<Network, String>,
    cookies: Arc<Mutex<CookieJar>>,
    fetch_policy: Option<FetchPolicy>,
    extra_http_headers: ExtraHttpHeaders,
    cache_disabled: CacheDisabledFlag,
    preflight_cache: Arc<Mutex<PreflightCache>>,
    permission_overrides: PermissionOverrides,
    runtime_interrupt: RuntimeInterruptHandle,
    active_fetches: ActiveFetches,
}

#[derive(Clone)]
pub(super) struct WebApiConfig {
    pub(super) network: NetworkConfig,
    pub(super) storage: WebStorageHost,
    pub(super) network_state: RuntimeNetworkState,
    pub(super) fetch_policy: Option<FetchPolicy>,
    pub(super) extra_http_headers: ExtraHttpHeaders,
    pub(super) cache_disabled: CacheDisabledFlag,
    pub(super) permission_overrides: PermissionOverrides,
    pub(super) interrupt: RuntimeInterruptHandle,
}

#[derive(Clone, Default)]
pub(super) struct RuntimeNetworkState {
    cookies: Arc<Mutex<CookieJar>>,
    preflight_cache: Arc<Mutex<PreflightCache>>,
}

impl RuntimeNetworkState {
    pub(super) fn clear(&self, cookies: bool, preflight_cache: bool) {
        if cookies && let Ok(mut jar) = self.cookies.lock() {
            *jar = CookieJar::default();
        }
        if preflight_cache && let Ok(mut cache) = self.preflight_cache.lock() {
            *cache = PreflightCache::default();
        }
    }

    pub(super) fn cookie_snapshots(&self) -> Result<Vec<CookieSnapshot>, String> {
        self.cookies
            .lock()
            .map(|jar| jar.snapshots())
            .map_err(|_| "cookie jar poisoned".to_owned())
    }

    pub(super) fn apply_cookie_delta(
        &self,
        delta: vixen_net::CookieJarDelta,
    ) -> Result<(), String> {
        self.cookies
            .lock()
            .map(|mut jar| jar.apply_delta(delta))
            .map_err(|_| "cookie jar poisoned".to_owned())
    }
}

type StorageEntries = Vec<(String, String)>;
type MemoryStorageMap = Arc<Mutex<HashMap<String, StorageEntries>>>;

impl WebApiHost {
    fn new(config: WebApiConfig) -> Self {
        Self {
            storage: config.storage,
            network: Network::new(config.network).map_err(|err| err.to_string()),
            cookies: config.network_state.cookies,
            fetch_policy: config.fetch_policy,
            extra_http_headers: config.extra_http_headers,
            cache_disabled: config.cache_disabled,
            preflight_cache: config.network_state.preflight_cache,
            permission_overrides: config.permission_overrides,
            runtime_interrupt: config.interrupt,
            active_fetches: ActiveFetches::default(),
        }
    }
}

impl Drop for WebApiHost {
    fn drop(&mut self) {
        self.active_fetches.cancel_all();
    }
}

const MAX_ACTIVE_FETCHES: usize = 32;

#[derive(Clone)]
struct FetchCancellation {
    cancelled: Arc<AtomicBool>,
    runtime_interrupt: RuntimeInterruptHandle,
    runtime_generation: u64,
}

impl FetchCancellation {
    fn new(runtime_interrupt: RuntimeInterruptHandle) -> Self {
        let runtime_generation = runtime_interrupt.fetch_generation();
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            runtime_interrupt,
            runtime_generation,
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self
                .runtime_interrupt
                .fetches_interrupted_since(self.runtime_generation)
    }
}

#[derive(Default)]
struct ActiveFetchState {
    next_id: u32,
    requests: BTreeMap<u32, ActiveFetch>,
}

struct ActiveFetch {
    cancellation: FetchCancellation,
    result: Option<tokio::sync::oneshot::Receiver<Value>>,
}

#[derive(Clone, Default)]
struct ActiveFetches(Arc<Mutex<ActiveFetchState>>);

impl ActiveFetches {
    fn start(&self, context: FetchHostContext, request: Value) -> Result<u32, String> {
        let cancellation = FetchCancellation::new(context.runtime_interrupt.clone());
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let id = {
            let mut state = self
                .0
                .lock()
                .map_err(|_| "active fetch state is poisoned".to_owned())?;
            if state.requests.len() >= MAX_ACTIVE_FETCHES {
                return Err(format!(
                    "active fetch request limit exceeded ({MAX_ACTIVE_FETCHES})"
                ));
            }
            let id = state.next_id;
            state.next_id = state
                .next_id
                .checked_add(1)
                .ok_or_else(|| "active fetch request id space is exhausted".to_owned())?;
            state.requests.insert(
                id,
                ActiveFetch {
                    cancellation: cancellation.clone(),
                    result: Some(result_rx),
                },
            );
            id
        };
        let worker_cancellation = cancellation.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("vixen-page-fetch".to_owned())
            .spawn(move || {
                let result = perform_fetch(context, request, worker_cancellation);
                let _ = result_tx.send(result);
            })
        {
            if let Ok(mut state) = self.0.lock() {
                state.requests.remove(&id);
            }
            return Err(format!("fetch worker spawn failed: {error}"));
        }
        Ok(id)
    }

    fn take_result(&self, id: u32) -> Result<tokio::sync::oneshot::Receiver<Value>, String> {
        self.0
            .lock()
            .map_err(|_| "active fetch state is poisoned".to_owned())?
            .requests
            .get_mut(&id)
            .ok_or_else(|| "unknown active fetch request".to_owned())?
            .result
            .take()
            .ok_or_else(|| "active fetch result is already being awaited".to_owned())
    }

    fn finish(&self, id: u32) {
        if let Ok(mut state) = self.0.lock() {
            state.requests.remove(&id);
        }
    }

    fn cancel(&self, id: u32) -> bool {
        self.0
            .lock()
            .ok()
            .and_then(|state| {
                state.requests.get(&id).map(|request| {
                    request.cancellation.cancel();
                })
            })
            .is_some()
    }

    fn cancel_all(&self) {
        let requests = self
            .0
            .lock()
            .map(|mut state| std::mem::take(&mut state.requests))
            .unwrap_or_default();
        for request in requests.into_values() {
            request.cancellation.cancel();
        }
    }
}

#[derive(Clone)]
struct FetchHostContext {
    network: Result<Network, String>,
    cookies: Arc<Mutex<CookieJar>>,
    cookie_store: Option<Arc<Store>>,
    fetch_policy: Option<FetchPolicy>,
    extra_http_headers: Vec<(String, String)>,
    cache_disabled: bool,
    preflight_cache: Arc<Mutex<PreflightCache>>,
    runtime_interrupt: RuntimeInterruptHandle,
}

#[derive(Clone, Default)]
pub(super) struct PermissionOverrides {
    grants: Arc<Mutex<HashMap<Option<String>, Vec<String>>>>,
}

impl PermissionOverrides {
    pub(super) fn replace(&self, origin: Option<String>, grants: Vec<String>) {
        if let Ok(mut entries) = self.grants.lock() {
            entries.insert(origin, grants);
        }
    }

    pub(super) fn reset(&self) {
        if let Ok(mut entries) = self.grants.lock() {
            entries.clear();
        }
    }

    fn decision(&self, origin: Option<&str>, kind: &str) -> Result<Option<bool>, String> {
        let entries = self
            .grants
            .lock()
            .map_err(|_| "permission override map poisoned".to_owned())?;
        let exact = origin
            .map(str::to_owned)
            .and_then(|origin| entries.get(&Some(origin)));
        let grants = exact.or_else(|| entries.get(&None));
        Ok(grants.map(|grants| grants.iter().any(|grant| grant == kind)))
    }
}

const MAX_PREFLIGHT_CACHE_ENTRIES: usize = 128;
const MAX_PREFLIGHT_CACHE_AGE: Duration = Duration::from_secs(2 * 60 * 60);
const DEFAULT_PREFLIGHT_CACHE_AGE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreflightCacheKey {
    request_origin: String,
    target_origin: String,
    credentials_mode: CorsCredentialsMode,
}

#[derive(Debug, Clone)]
struct PreflightCacheEntry {
    key: PreflightCacheKey,
    allow_methods: Vec<String>,
    allow_headers: Vec<String>,
    expires_at: Instant,
}

#[derive(Debug, Default)]
struct PreflightCache {
    entries: VecDeque<PreflightCacheEntry>,
}

impl PreflightCache {
    fn allows(
        &mut self,
        key: &PreflightCacheKey,
        method: Method,
        unsafe_header_names: &[String],
        now: Instant,
    ) -> bool {
        self.entries.retain(|entry| entry.expires_at > now);
        self.entries.iter().any(|entry| {
            entry.key == *key
                && preflight_method_allowed(&entry.allow_methods, method, key.credentials_mode)
                && preflight_headers_allowed(
                    &entry.allow_headers,
                    unsafe_header_names,
                    key.credentials_mode,
                )
        })
    }

    fn insert(&mut self, entry: PreflightCacheEntry) {
        self.entries.push_back(entry);
        while self.entries.len() > MAX_PREFLIGHT_CACHE_ENTRIES {
            self.entries.pop_front();
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct ExtraHttpHeaders {
    entries: Arc<Mutex<Vec<(String, String)>>>,
}

#[derive(Clone, Default)]
pub(super) struct CacheDisabledFlag {
    value: Arc<Mutex<bool>>,
}

impl CacheDisabledFlag {
    pub(super) fn set(&self, disabled: bool) {
        if let Ok(mut guard) = self.value.lock() {
            *guard = disabled;
        }
    }

    pub(super) fn snapshot(&self) -> bool {
        self.value.lock().map(|guard| *guard).unwrap_or(false)
    }
}

impl ExtraHttpHeaders {
    pub(super) fn set(&self, entries: Vec<(String, String)>) {
        if let Ok(mut guard) = self.entries.lock() {
            *guard = entries;
        }
    }

    fn snapshot(&self) -> Vec<(String, String)> {
        self.entries
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone)]
pub(super) struct FetchPolicy {
    csp: ContentSecurityPolicy,
    origin: Origin,
    context_url: Option<Url>,
}

impl FetchPolicy {
    pub(super) fn from_page(page: &Page) -> Self {
        let mut csp = page.csp().clone();
        for item in page.document().script_execution_items() {
            if let DocumentScriptItem::CspMeta(policy) = item {
                csp.add_header(&policy);
            }
        }
        let context_url = Url::parse(page.url()).ok();
        let origin = context_url
            .as_ref()
            .map(Origin::from_url)
            .unwrap_or_else(Origin::opaque);
        Self {
            csp,
            origin,
            context_url,
        }
    }

    fn allows_connect(&self, url: &Url) -> bool {
        self.csp.allows_fetch("connect-src", url, &self.origin)
    }

    fn mixed_content_verdict(&self, url: &Url) -> MixedContentVerdict {
        let context_trustworthy = self
            .context_url
            .as_ref()
            .is_some_and(is_potentially_trustworthy);
        classify_mixed_content(context_trustworthy, url, ResourceType::Fetch, false)
    }

    fn is_cross_origin(&self, url: &Url) -> bool {
        Origin::from_url(url) != self.origin
    }

    fn cors_origin(&self) -> String {
        cors_origin_value(&self.origin)
    }

    fn referrer_header(
        &self,
        url: &Url,
        policy_text: Option<&str>,
    ) -> Result<Option<String>, String> {
        let Some(source) = self.context_url.as_ref() else {
            return Ok(None);
        };
        let policy = match policy_text.filter(|value| !value.is_empty()) {
            Some(value) => parse_referrer_policy(value)
                .ok_or_else(|| format!("unsupported fetch referrer policy: {value}"))?,
            None => ReferrerPolicy::default(),
        };
        Ok(match resolve_referrer(policy, source, url) {
            ReferrerValue::None => None,
            ReferrerValue::Origin(value) | ReferrerValue::FullUrl(value) => Some(value),
        })
    }
}

fn cors_origin_value(origin: &Origin) -> String {
    if origin.is_opaque() {
        return "null".to_owned();
    }
    match (origin.scheme(), origin.host(), origin.port()) {
        ("http", host, Some(80)) | ("https", host, Some(443)) => {
            format!("{}://{}", origin.scheme(), host)
        }
        (_, host, Some(port)) => format!("{}://{}:{}", origin.scheme(), host, port),
        (_, host, None) => format!("{}://{}", origin.scheme(), host),
    }
}

#[derive(Clone)]
pub(super) struct WebStorageHost {
    backend: WebStorageBackend,
    local_partition_key: String,
    session_partition_key: String,
}

impl WebStorageHost {
    pub(super) fn new(backend: WebStorageBackend, partitions: WebStoragePartitions) -> Self {
        Self {
            backend,
            local_partition_key: partitions.local,
            session_partition_key: partitions.session,
        }
    }
}

#[derive(Clone)]
pub(super) struct WebStoragePartitions {
    pub(super) local: String,
    pub(super) session: String,
}

#[derive(Clone)]
pub(super) enum WebStorageBackend {
    Memory(MemoryStorageMap),
    Store(Arc<Store>),
}

impl WebStorageBackend {
    pub(super) fn memory() -> Self {
        Self::Memory(Arc::new(Mutex::new(HashMap::new())))
    }

    pub(super) fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        Store::open(path)
            .map(|store| Self::Store(Arc::new(store)))
            .map_err(|err| err.to_string())
    }

    pub(super) fn from_store(store: Arc<Store>) -> Self {
        Self::Store(store)
    }

    pub(super) fn store(&self) -> Option<Arc<Store>> {
        match self {
            Self::Store(store) => Some(Arc::clone(store)),
            Self::Memory(_) => None,
        }
    }
}

deno_core::extension!(
    vixen_webapi,
    ops = [
        op_vixen_storage_length,
        op_vixen_storage_key,
        op_vixen_storage_get,
        op_vixen_storage_set,
        op_vixen_storage_remove,
        op_vixen_storage_clear,
        op_vixen_storage_estimate,
        op_vixen_storage_persisted,
        op_vixen_document_cookie_get,
        op_vixen_document_cookie_set,
        op_vixen_permission_query,
        op_vixen_crypto_random_bytes,
        op_vixen_fetch_start,
        op_vixen_fetch_finish,
        op_vixen_fetch_cancel,
    ],
);

pub(super) fn extension(config: WebApiConfig) -> Extension {
    let mut extension = vixen_webapi::init();
    extension.op_state_fn = Some(Box::new(move |state| {
        state.put(WebApiHost::new(config.clone()));
    }));
    extension.js_files = Cow::Owned(vec![ExtensionFileSource::new_computed(
        "ext:vixen_webapi/bootstrap.js",
        Arc::<str>::from(WEB_API_BOOTSTRAP),
    )]);
    extension
}

#[deno_core::op2(fast)]
fn op_vixen_storage_length(state: &mut OpState, #[string] kind: &str) -> u32 {
    let host = state.borrow::<WebApiHost>();
    let Some(kind) = parse_storage_kind(kind) else {
        return 0;
    };
    storage_entries(host, kind)
        .map(|entries| entries.len() as u32)
        .unwrap_or(0)
}

#[deno_core::op2]
#[serde]
fn op_vixen_storage_key(
    state: &mut OpState,
    #[string] kind: &str,
    index: u32,
) -> deno_core::serde_json::Value {
    let Some(kind) = parse_storage_kind(kind) else {
        return storage_error("unsupported Storage kind");
    };
    let host = state.borrow::<WebApiHost>();
    let entries = match storage_entries(host, kind) {
        Ok(entries) => entries,
        Err(message) => return storage_error(message),
    };
    json!({
        "ok": true,
        "value": entries
            .get(index as usize)
            .map(|(key, _)| key.clone()),
    })
}

#[deno_core::op2]
#[serde]
fn op_vixen_storage_get(
    state: &mut OpState,
    #[string] kind: &str,
    #[string] key: String,
) -> deno_core::serde_json::Value {
    let Some(kind) = parse_storage_kind(kind) else {
        return storage_error("unsupported Storage kind");
    };
    if key.is_empty() {
        return storage_value(None);
    }
    if let Err(err) = validate_storage_key(&key) {
        return storage_key_error(err);
    }
    let host = state.borrow::<WebApiHost>();
    match storage_get_item(host, kind, &key) {
        Ok(value) => storage_value(value),
        Err(message) => storage_error(message),
    }
}

#[deno_core::op2]
#[serde]
fn op_vixen_storage_set(
    state: &mut OpState,
    #[string] kind: &str,
    #[string] key: String,
    #[string] value: String,
) -> deno_core::serde_json::Value {
    let Some(kind) = parse_storage_kind(kind) else {
        return storage_error("unsupported Storage kind");
    };
    if let Err(err) = validate_storage_key(&key) {
        return storage_key_error(err);
    }
    if let Err(err) = validate_storage_value(&value) {
        return storage_key_error(err);
    }

    let host = state.borrow::<WebApiHost>();
    let entries = match storage_entries(host, kind) {
        Ok(entries) => entries,
        Err(message) => return storage_error(message),
    };
    if let Err(err) = storage_check_quota(&entries, &key, &value) {
        return storage_quota_error(err);
    }

    if let Err(message) = storage_set_item(host, kind, key, value) {
        return storage_error(message);
    }
    json!({ "ok": true })
}

#[deno_core::op2]
#[serde]
fn op_vixen_fetch_start(
    state: Rc<RefCell<OpState>>,
    #[serde] request: deno_core::serde_json::Value,
) -> deno_core::serde_json::Value {
    let (active_fetches, context) = {
        let state = state.borrow();
        let host = state.borrow::<WebApiHost>();
        (
            host.active_fetches.clone(),
            FetchHostContext {
                network: host.network.clone(),
                cookies: host.cookies.clone(),
                cookie_store: web_storage_store(&host.storage),
                fetch_policy: host.fetch_policy.clone(),
                extra_http_headers: host.extra_http_headers.snapshot(),
                cache_disabled: host.cache_disabled.snapshot(),
                preflight_cache: host.preflight_cache.clone(),
                runtime_interrupt: host.runtime_interrupt.clone(),
            },
        )
    };
    match active_fetches.start(context, request) {
        Ok(id) => json!({ "ok": true, "id": id }),
        Err(message) => fetch_error(message),
    }
}

#[deno_core::op2]
#[serde]
async fn op_vixen_fetch_finish(
    state: Rc<RefCell<OpState>>,
    id: u32,
) -> deno_core::serde_json::Value {
    let active_fetches = {
        let state = state.borrow();
        state.borrow::<WebApiHost>().active_fetches.clone()
    };
    let result = match active_fetches.take_result(id) {
        Ok(result) => result,
        Err(message) => return fetch_error(message),
    };
    let result = result
        .await
        .unwrap_or_else(|_| fetch_error("fetch worker closed without a result"));
    active_fetches.finish(id);
    result
}

#[deno_core::op2(fast)]
fn op_vixen_fetch_cancel(state: &mut OpState, id: u32) -> bool {
    state.borrow::<WebApiHost>().active_fetches.cancel(id)
}

fn perform_fetch(
    context: FetchHostContext,
    request: deno_core::serde_json::Value,
    cancellation: FetchCancellation,
) -> deno_core::serde_json::Value {
    let Some(url_text) = request.get("url").and_then(Value::as_str) else {
        return fetch_error("fetch request missing URL");
    };
    let method_text = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET");
    let method = match parse_fetch_method(method_text) {
        Ok(method) => method,
        Err(message) => return fetch_error(message),
    };
    let cache_mode = match parse_fetch_cache(
        request
            .get("cache")
            .and_then(Value::as_str)
            .unwrap_or("default"),
    ) {
        Ok(mode) => mode,
        Err(message) => return fetch_error(message),
    };
    let fetch_mode = match parse_fetch_mode(
        request
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("cors"),
    ) {
        Ok(mode) => mode,
        Err(message) => return fetch_error(message),
    };
    let credentials_mode = match parse_fetch_credentials(
        request
            .get("credentials")
            .and_then(Value::as_str)
            .unwrap_or("same-origin"),
    ) {
        Ok(mode) => mode,
        Err(message) => return fetch_error(message),
    };
    let redirect_mode = match parse_fetch_redirect(
        request
            .get("redirect")
            .and_then(Value::as_str)
            .unwrap_or("follow"),
    ) {
        Ok(mode) => mode,
        Err(message) => return fetch_error(message),
    };
    let mut headers = match parse_fetch_headers(request.get("headers")) {
        Ok(headers) => headers,
        Err(message) => return fetch_error(message),
    };
    let integrity = request
        .get("integrity")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let body = request
        .get("body")
        .and_then(Value::as_str)
        .map(|value| value.as_bytes().to_vec());
    let url = match Url::parse(url_text) {
        Ok(url) => url,
        Err(err) => {
            return fetch_failure(
                url_text,
                method,
                format!("invalid URL: {err}"),
                "url-policy",
            );
        }
    };
    if let Err(err) = validate_http_url(&url) {
        return fetch_failure(
            url.as_str(),
            method,
            format!("URL rejected by policy: {err}"),
            "url-policy",
        );
    }

    let FetchHostContext {
        network,
        cookies,
        cookie_store,
        fetch_policy,
        extra_http_headers,
        cache_disabled,
        preflight_cache,
        runtime_interrupt,
    } = context;
    let network = match network {
        Ok(network) => network.clone(),
        Err(message) => {
            return fetch_failure(
                url.as_str(),
                method,
                format!("network unavailable: {message}"),
                "network",
            );
        }
    };
    headers.extend(extra_http_headers);
    let author_headers = headers.clone();

    if let Some(policy) = &fetch_policy
        && !policy.allows_connect(&url)
    {
        return fetch_failure(
            url.as_str(),
            method,
            "fetch blocked by CSP connect-src",
            "csp",
        );
    }
    if let Some(policy) = &fetch_policy
        && policy.mixed_content_verdict(&url) == MixedContentVerdict::Block
    {
        return fetch_failure(
            url.as_str(),
            method,
            "fetch blocked as active mixed content",
            "mixed-content",
        );
    }
    if let Some(policy) = &fetch_policy {
        match policy.referrer_header(&url, request.get("referrerPolicy").and_then(Value::as_str)) {
            Ok(Some(value)) => headers.push(("referer".to_owned(), value)),
            Ok(None) => {}
            Err(message) => return fetch_failure(url.as_str(), method, message, "policy"),
        }
    }
    let cross_origin = fetch_policy
        .as_ref()
        .is_some_and(|policy| policy.is_cross_origin(&url));
    if fetch_mode == FetchMode::SameOrigin && cross_origin {
        return fetch_failure(
            url.as_str(),
            method,
            "fetch blocked by mode same-origin",
            "origin",
        );
    }
    if cross_origin
        && matches!(fetch_mode, FetchMode::Cors | FetchMode::NoCors)
        && let Some(policy) = &fetch_policy
    {
        headers.push(("origin".to_owned(), policy.cors_origin()));
    }
    if cross_origin
        && fetch_mode == FetchMode::Cors
        && let Some(policy) = &fetch_policy
    {
        let unsafe_header_names = cors_unsafe_request_header_names(&author_headers);
        if cors_preflight_required(method, &unsafe_header_names)
            && let Err(message) = cors_preflight_blocking(
                network.clone(),
                CorsPreflightRequest {
                    url: url.clone(),
                    method,
                    origin: policy.cors_origin(),
                    unsafe_header_names,
                    credentials_mode: credentials_mode.cors_mode(),
                },
                preflight_cache,
                runtime_interrupt.clone(),
                cancellation.clone(),
            )
        {
            return fetch_failure(url.as_str(), method, message, "cors");
        }
    }
    let send_credentials = credentials_mode.sends_credentials(cross_origin);
    let origin_key = cookie_origin_key(&url);
    let origin_host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let mut request_cookies = if send_credentials {
        match load_cookie_jar(&cookies, cookie_store.as_deref(), &origin_key) {
            Ok(jar) => jar,
            Err(message) => return fetch_failure(url.as_str(), method, message, "profile"),
        }
    } else {
        CookieJar::default()
    };
    let cache_request = TextRequest {
        url: url.clone(),
        cross_site: cross_origin,
        method,
        redirect_mode,
        headers: headers.clone(),
        body: body.clone(),
    };
    let effective_headers =
        match network.effective_request_headers(&mut request_cookies, &cache_request) {
            Ok(headers) => headers,
            Err(error) => return fetch_failure(url.as_str(), method, error.to_string(), "network"),
        };
    let cached = if !cache_disabled && method == Method::Get {
        match fetch_cache_lookup(
            cookie_store.as_deref(),
            &url,
            &effective_headers,
            network.config().max_body_bytes,
        ) {
            Ok(candidate) => candidate,
            Err(message) => return fetch_failure(url.as_str(), method, message, "cache"),
        }
    } else {
        None
    };

    if cache_disabled && method == Method::Get && cache_mode == FetchCacheMode::OnlyIfCached {
        return fetch_failure(url.as_str(), method, "fetch cache disabled", "cache");
    }

    if !cache_disabled
        && method == Method::Get
        && matches!(
            cache_mode,
            FetchCacheMode::ForceCache | FetchCacheMode::OnlyIfCached
        )
    {
        match cached.clone() {
            Some((mut response, _)) => {
                response.events =
                    cached_response_events(&url, response.status, response.body.len());
                let response = match apply_fetch_integrity(response, &integrity) {
                    Ok(response) => response,
                    Err(message) => {
                        return fetch_failure(url.as_str(), method, message, "integrity");
                    }
                };
                return match apply_fetch_visibility(
                    response,
                    fetch_mode,
                    credentials_mode,
                    fetch_policy.as_ref(),
                    &url,
                ) {
                    Ok((response, response_type)) => fetch_response(response, response_type),
                    Err(message) => fetch_error(message),
                };
            }
            None if cache_mode == FetchCacheMode::OnlyIfCached => {
                return fetch_failure(url.as_str(), method, "fetch cache miss", "cache");
            }
            None => {}
        }
    }

    if cache_mode == FetchCacheMode::Default
        && let Some((mut response, crate::http_cache::CacheUse::Fresh)) = cached.clone()
    {
        response.events = cached_response_events(&url, response.status, response.body.len());
        let response = match apply_fetch_integrity(response, &integrity) {
            Ok(response) => response,
            Err(message) => return fetch_failure(url.as_str(), method, message, "integrity"),
        };
        return match apply_fetch_visibility(
            response,
            fetch_mode,
            credentials_mode,
            fetch_policy.as_ref(),
            &url,
        ) {
            Ok((response, response_type)) => fetch_response(response, response_type),
            Err(message) => {
                let reason = fetch_blocked_reason(&message);
                fetch_failure(url.as_str(), method, message, reason)
            }
        };
    }

    let revalidation_candidate = match cache_mode {
        FetchCacheMode::NoCache => cached
            .filter(|(response, _)| text_response_has_validator(response))
            .map(|(response, _)| response),
        FetchCacheMode::Default => cached
            .filter(|(response, cache_use)| {
                *cache_use == crate::http_cache::CacheUse::Stale
                    && text_response_has_validator(response)
            })
            .map(|(response, _)| response),
        _ => None,
    };
    if let Some(candidate) = &revalidation_candidate {
        add_cache_revalidation_headers(&mut headers, candidate);
    }

    match fetch_http_text_blocking(
        network,
        TextRequest {
            url: url.clone(),
            cross_site: cross_origin,
            method,
            redirect_mode,
            headers,
            body,
        },
        FetchWorkerProfile {
            credentials: send_credentials.then_some(FetchRequestCredentials {
                jar: request_cookies,
                origin_key,
                origin_host,
            }),
            cookies,
            store: cookie_store.clone(),
        },
        runtime_interrupt.clone(),
        cancellation.clone(),
    ) {
        Ok(response) => {
            let response = revalidated_response(response, revalidation_candidate);
            let response = match apply_fetch_integrity(response, &integrity) {
                Ok(response) => response,
                Err(message) => {
                    return fetch_failure(url.as_str(), method, message, "integrity");
                }
            };
            if !cache_disabled
                && method == Method::Get
                && cache_mode != FetchCacheMode::NoStore
                && response.status != 304
                && let Err(message) = commit_host_effect(&runtime_interrupt, &cancellation, || {
                    persist_fetch_cache(
                        cookie_store.as_deref(),
                        &cookie_origin_key(&url),
                        &response,
                    )
                })
            {
                return fetch_failure(url.as_str(), method, message, "cache");
            }
            match apply_fetch_visibility(
                response,
                fetch_mode,
                credentials_mode,
                fetch_policy.as_ref(),
                &url,
            ) {
                Ok((response, response_type)) => fetch_response(response, response_type),
                Err(message) => {
                    let reason = fetch_blocked_reason(&message);
                    fetch_failure(url.as_str(), method, message, reason)
                }
            }
        }
        Err(message) => {
            let reason = fetch_blocked_reason(&message);
            fetch_failure(url.as_str(), method, message, reason)
        }
    }
}

#[deno_core::op2]
#[serde]
fn op_vixen_storage_remove(
    state: &mut OpState,
    #[string] kind: &str,
    #[string] key: String,
) -> deno_core::serde_json::Value {
    let Some(kind) = parse_storage_kind(kind) else {
        return storage_error("unsupported Storage kind");
    };
    if key.is_empty() {
        return json!({ "ok": true });
    }
    if let Err(err) = validate_storage_key(&key) {
        return storage_key_error(err);
    }

    let host = state.borrow::<WebApiHost>();
    match storage_remove_item(host, kind, &key) {
        Ok(()) => json!({ "ok": true }),
        Err(message) => storage_error(message),
    }
}

#[deno_core::op2]
#[serde]
fn op_vixen_storage_clear(
    state: &mut OpState,
    #[string] kind: &str,
) -> deno_core::serde_json::Value {
    let Some(kind) = parse_storage_kind(kind) else {
        return storage_error("unsupported Storage kind");
    };
    let host = state.borrow::<WebApiHost>();
    match storage_clear(host, kind) {
        Ok(()) => json!({ "ok": true }),
        Err(message) => storage_error(message),
    }
}

#[deno_core::op2]
#[serde]
fn op_vixen_storage_estimate(state: &mut OpState) -> deno_core::serde_json::Value {
    let host = state.borrow::<WebApiHost>();
    let usage = storage_entries(host, StorageKind::Local)
        .map(|entries| storage_total_bytes(&entries))
        .unwrap_or(0);
    json!({
        "ok": true,
        "usage": usage,
        "quota": MAX_PARTITION_BYTES,
    })
}

#[deno_core::op2(fast)]
fn op_vixen_storage_persisted(state: &mut OpState) -> bool {
    let host = state.borrow::<WebApiHost>();
    permission_state(host, "persistent-storage").is_ok_and(|state| state == "granted")
}

#[deno_core::op2]
#[serde]
fn op_vixen_permission_query(
    state: &mut OpState,
    #[string] name: String,
) -> deno_core::serde_json::Value {
    let Some(kind) = canonical_permission_name(&name) else {
        return json!({
            "ok": false,
            "message": format!("unsupported permission name: {name}"),
        });
    };
    let host = state.borrow::<WebApiHost>();
    match permission_state(host, kind) {
        Ok(state) => json!({ "ok": true, "state": state }),
        Err(message) => json!({ "ok": false, "message": message }),
    }
}

#[deno_core::op2]
#[serde]
fn op_vixen_crypto_random_bytes(len: u32) -> deno_core::serde_json::Value {
    if len > 65_536 {
        return json!({
            "ok": false,
            "message": "Crypto.getRandomValues quota exceeded",
        });
    }
    let mut bytes = vec![0; len as usize];
    match getrandom::fill(&mut bytes) {
        Ok(()) => json!({ "ok": true, "bytes": bytes }),
        Err(err) => json!({
            "ok": false,
            "message": format!("secure random source unavailable: {err}"),
        }),
    }
}

#[deno_core::op2]
#[serde]
fn op_vixen_document_cookie_get(
    state: &mut OpState,
    #[string] url_text: String,
) -> deno_core::serde_json::Value {
    let Ok(url) = Url::parse(&url_text) else {
        return storage_error("invalid document URL for cookie read");
    };
    if !matches!(url.scheme(), "http" | "https") {
        return storage_value(Some(String::new()));
    }

    let host = state.borrow::<WebApiHost>();
    let store = web_storage_store(&host.storage);
    let origin_key = cookie_origin_key(&url);
    match load_cookie_jar(&host.cookies, store.as_deref(), &origin_key) {
        Ok(jar) => storage_value(Some(jar.document_cookie_string(&url))),
        Err(message) => storage_error(message),
    }
}

#[deno_core::op2]
#[serde]
fn op_vixen_document_cookie_set(
    state: &mut OpState,
    #[string] url_text: String,
    #[string] value: String,
) -> deno_core::serde_json::Value {
    let Ok(url) = Url::parse(&url_text) else {
        return storage_error("invalid document URL for cookie write");
    };
    if !matches!(url.scheme(), "http" | "https") {
        return json!({ "ok": true });
    }

    let host = state.borrow::<WebApiHost>();
    let store = web_storage_store(&host.storage);
    let origin_key = cookie_origin_key(&url);
    let origin_host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let mut jar = match load_cookie_jar(&host.cookies, store.as_deref(), &origin_key) {
        Ok(jar) => jar,
        Err(message) => return storage_error(message),
    };
    if let Err(err) = jar.set_cookie(&value, &url, false) {
        return storage_error(err.to_string());
    }
    match persist_cookie_jar(
        &host.cookies,
        store.as_deref(),
        &origin_key,
        &origin_host,
        jar,
    ) {
        Ok(()) => json!({ "ok": true }),
        Err(message) => storage_error(message),
    }
}

fn parse_storage_kind(kind: &str) -> Option<StorageKind> {
    match kind {
        "local" => Some(StorageKind::Local),
        "session" => Some(StorageKind::Session),
        _ => None,
    }
}

fn parse_fetch_method(method: &str) -> Result<Method, String> {
    match method.to_ascii_uppercase().as_str() {
        "GET" => Ok(Method::Get),
        "HEAD" => Ok(Method::Head),
        "POST" => Ok(Method::Post),
        "PUT" => Ok(Method::Put),
        "DELETE" => Ok(Method::Delete),
        "PATCH" => Ok(Method::Patch),
        "OPTIONS" => Ok(Method::Options),
        other => Err(format!("unsupported fetch method: {other}")),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FetchCacheMode {
    Default,
    NoStore,
    Reload,
    NoCache,
    ForceCache,
    OnlyIfCached,
}

fn parse_fetch_cache(mode: &str) -> Result<FetchCacheMode, String> {
    match mode {
        "default" => Ok(FetchCacheMode::Default),
        "no-store" => Ok(FetchCacheMode::NoStore),
        "reload" => Ok(FetchCacheMode::Reload),
        "no-cache" => Ok(FetchCacheMode::NoCache),
        "force-cache" => Ok(FetchCacheMode::ForceCache),
        "only-if-cached" => Ok(FetchCacheMode::OnlyIfCached),
        other => Err(format!("unsupported fetch cache mode: {other}")),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FetchMode {
    SameOrigin,
    Cors,
    NoCors,
}

fn parse_fetch_mode(mode: &str) -> Result<FetchMode, String> {
    match mode {
        "same-origin" => Ok(FetchMode::SameOrigin),
        "cors" => Ok(FetchMode::Cors),
        "no-cors" => Ok(FetchMode::NoCors),
        other => Err(format!("unsupported fetch mode: {other}")),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FetchCredentialsMode {
    Omit,
    SameOrigin,
    Include,
}

impl FetchCredentialsMode {
    fn sends_credentials(self, cross_origin: bool) -> bool {
        match self {
            FetchCredentialsMode::Omit => false,
            FetchCredentialsMode::SameOrigin => !cross_origin,
            FetchCredentialsMode::Include => true,
        }
    }

    fn cors_mode(self) -> CorsCredentialsMode {
        match self {
            FetchCredentialsMode::Include => CorsCredentialsMode::Include,
            FetchCredentialsMode::Omit | FetchCredentialsMode::SameOrigin => {
                CorsCredentialsMode::Omit
            }
        }
    }
}

fn parse_fetch_credentials(mode: &str) -> Result<FetchCredentialsMode, String> {
    match mode {
        "omit" => Ok(FetchCredentialsMode::Omit),
        "same-origin" => Ok(FetchCredentialsMode::SameOrigin),
        "include" => Ok(FetchCredentialsMode::Include),
        other => Err(format!("unsupported fetch credentials mode: {other}")),
    }
}

fn parse_fetch_redirect(mode: &str) -> Result<RedirectMode, String> {
    match mode {
        "follow" => Ok(RedirectMode::Follow),
        "error" => Ok(RedirectMode::Error),
        "manual" => Ok(RedirectMode::Manual),
        other => Err(format!("unsupported fetch redirect mode: {other}")),
    }
}

fn parse_fetch_headers(value: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    let Some(Value::Array(entries)) = value else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let Value::Array(pair) = entry else {
            return Err("fetch headers must be [name, value] pairs".to_owned());
        };
        if pair.len() != 2 {
            return Err("fetch headers must be [name, value] pairs".to_owned());
        }
        let Some(name) = pair[0].as_str() else {
            return Err("fetch header name must be a string".to_owned());
        };
        let Some(value) = pair[1].as_str() else {
            return Err("fetch header value must be a string".to_owned());
        };
        out.push((name.to_owned(), value.to_owned()));
    }
    Ok(out)
}

fn cors_unsafe_request_header_names(headers: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if !is_cors_safelisted_request_header(&lower, value) && !out.contains(&lower) {
            out.push(lower);
        }
    }
    out
}

fn cors_preflight_required(method: Method, unsafe_header_names: &[String]) -> bool {
    !matches!(method, Method::Get | Method::Head | Method::Post) || !unsafe_header_names.is_empty()
}

struct CorsPreflightRequest {
    url: Url,
    method: Method,
    origin: String,
    unsafe_header_names: Vec<String>,
    credentials_mode: CorsCredentialsMode,
}

fn cors_preflight_blocking(
    network: Network,
    request: CorsPreflightRequest,
    cache: Arc<Mutex<PreflightCache>>,
    runtime_interrupt: RuntimeInterruptHandle,
    cancellation: FetchCancellation,
) -> Result<(), String> {
    let CorsPreflightRequest {
        url,
        method,
        origin: request_origin,
        unsafe_header_names,
        credentials_mode,
    } = request;
    let key = PreflightCacheKey {
        request_origin: request_origin.clone(),
        target_origin: Origin::from_url(&url).partition_key(),
        credentials_mode,
    };
    if cache
        .lock()
        .map_err(|_| "CORS preflight cache poisoned".to_owned())?
        .allows(&key, method, &unsafe_header_names, Instant::now())
    {
        return Ok(());
    }

    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let handle = std::thread::Builder::new()
        .name("vixen-fetch-preflight".to_owned())
        .spawn(move || {
            let result = (|| {
                let mut headers = vec![
                    ("origin".to_owned(), request_origin.clone()),
                    (
                        "access-control-request-method".to_owned(),
                        method.as_str().to_owned(),
                    ),
                ];
                if !unsafe_header_names.is_empty() {
                    headers.push((
                        "access-control-request-headers".to_owned(),
                        unsafe_header_names.join(", "),
                    ));
                }
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| format!("network runtime unavailable: {err}"))?;
                let mut network = network;
                let mut jar = CookieJar::default();
                let response = rt.block_on(async {
                    let transport = Box::pin(network.get_text_with_cookies_request(
                        &mut jar,
                        TextRequest {
                            url,
                            cross_site: true,
                            method: Method::Options,
                            redirect_mode: RedirectMode::Error,
                            headers,
                            body: None,
                        },
                    ));
                    let cancel = Box::pin(cancel_rx);
                    match select(transport, cancel).await {
                        Either::Left((result, _)) => result.map_err(|err| err.to_string()),
                        Either::Right((_, transport)) => {
                            drop(transport);
                            Err(HOST_CALL_INTERRUPTED.to_owned())
                        }
                    }
                })?;
                validate_cors_preflight_response(
                    response,
                    &request_origin,
                    method,
                    &unsafe_header_names,
                    credentials_mode,
                )
            })();
            let _ = result_tx.send(result);
        })
        .map_err(|err| format!("fetch preflight worker spawn failed: {err}"))?;
    let cors_headers = wait_for_host_worker(
        result_rx,
        handle,
        cancel_tx,
        &runtime_interrupt,
        &cancellation,
        "fetch preflight",
    )?;
    let max_age = Duration::from_secs(
        cors_headers
            .max_age
            .unwrap_or(DEFAULT_PREFLIGHT_CACHE_AGE.as_secs()),
    )
    .min(MAX_PREFLIGHT_CACHE_AGE);
    if !max_age.is_zero() {
        commit_host_effect(&runtime_interrupt, &cancellation, || {
            cache
                .lock()
                .map_err(|_| "CORS preflight cache poisoned".to_owned())?
                .insert(PreflightCacheEntry {
                    key,
                    allow_methods: cors_headers.allow_methods,
                    allow_headers: cors_headers.allow_headers,
                    expires_at: Instant::now() + max_age,
                });
            Ok(())
        })?;
    }
    Ok(())
}

fn validate_cors_preflight_response(
    response: TextResponse,
    request_origin: &str,
    method: Method,
    unsafe_header_names: &[String],
    credentials_mode: CorsCredentialsMode,
) -> Result<CorsResponseHeaders, String> {
    if !(200..=299).contains(&response.status) {
        return Err(format!(
            "fetch blocked by CORS preflight: HTTP {}",
            response.status
        ));
    }
    let cors_headers = CorsResponseHeaders::from_headers(
        response
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    match cors_check(&cors_headers, request_origin, credentials_mode) {
        CorsCheckOutcome::Pass => {}
        CorsCheckOutcome::Fail(err) => {
            return Err(format!("fetch blocked by CORS preflight: {err}"));
        }
    }
    if !preflight_method_allowed(&cors_headers.allow_methods, method, credentials_mode) {
        return Err(format!(
            "fetch blocked by CORS preflight: method {} not allowed",
            method.as_str()
        ));
    }
    if !preflight_headers_allowed(
        &cors_headers.allow_headers,
        unsafe_header_names,
        credentials_mode,
    ) {
        return Err("fetch blocked by CORS preflight: request header not allowed".to_owned());
    }
    Ok(cors_headers)
}

fn preflight_method_allowed(
    allowed_methods: &[String],
    method: Method,
    credentials_mode: CorsCredentialsMode,
) -> bool {
    (credentials_mode != CorsCredentialsMode::Include
        && allowed_methods.iter().any(|value| value == "*"))
        || allowed_methods
            .iter()
            .any(|value| value.eq_ignore_ascii_case(method.as_str()))
}

fn preflight_headers_allowed(
    allowed_headers: &[String],
    unsafe_header_names: &[String],
    credentials_mode: CorsCredentialsMode,
) -> bool {
    let wildcard = credentials_mode != CorsCredentialsMode::Include
        && allowed_headers.iter().any(|value| value == "*");
    unsafe_header_names.iter().all(|name| {
        wildcard
            || allowed_headers
                .iter()
                .any(|value| value.eq_ignore_ascii_case(name))
    })
}

fn fetch_http_text_blocking(
    network: Network,
    request: TextRequest,
    profile: FetchWorkerProfile,
    runtime_interrupt: RuntimeInterruptHandle,
    cancellation: FetchCancellation,
) -> Result<TextResponse, String> {
    let persistence_cookies = Arc::clone(&profile.cookies);
    let persistence_store = profile.store.clone();
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let handle = std::thread::Builder::new()
        .name("vixen-fetch".to_owned())
        .spawn(move || {
            let result = (|| {
                let mut credentials = profile.credentials;
                let mut jar = credentials
                    .as_mut()
                    .map_or_else(CookieJar::default, |credentials| {
                        std::mem::take(&mut credentials.jar)
                    });
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| format!("network runtime unavailable: {err}"))?;
                let mut network = network;
                let response = rt.block_on(async {
                    let transport =
                        Box::pin(network.get_text_with_cookies_request(&mut jar, request));
                    let cancel = Box::pin(cancel_rx);
                    match select(transport, cancel).await {
                        Either::Left((result, _)) => result.map_err(|err| err.to_string()),
                        Either::Right((_, transport)) => {
                            drop(transport);
                            Err(HOST_CALL_INTERRUPTED.to_owned())
                        }
                    }
                })?;
                Ok(FetchWorkerResult {
                    response,
                    credentials: credentials.map(|credentials| FetchWorkerCredentials {
                        origin_key: credentials.origin_key,
                        origin_host: credentials.origin_host,
                        jar,
                    }),
                })
            })();
            let _ = result_tx.send(result);
        })
        .map_err(|err| format!("fetch worker spawn failed: {err}"))?;
    let result = wait_for_host_worker(
        result_rx,
        handle,
        cancel_tx,
        &runtime_interrupt,
        &cancellation,
        "fetch",
    )?;
    if let Some(credentials) = result.credentials {
        commit_host_effect(&runtime_interrupt, &cancellation, || {
            persist_cookie_jar(
                &persistence_cookies,
                persistence_store.as_deref(),
                &credentials.origin_key,
                &credentials.origin_host,
                credentials.jar,
            )
        })?;
    }
    Ok(result.response)
}

struct FetchWorkerProfile {
    credentials: Option<FetchRequestCredentials>,
    cookies: Arc<Mutex<CookieJar>>,
    store: Option<Arc<Store>>,
}

struct FetchRequestCredentials {
    jar: CookieJar,
    origin_key: String,
    origin_host: String,
}

struct FetchWorkerResult {
    response: TextResponse,
    credentials: Option<FetchWorkerCredentials>,
}

struct FetchWorkerCredentials {
    origin_key: String,
    origin_host: String,
    jar: CookieJar,
}

const HOST_WORKER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const HOST_CALL_INTERRUPTED: &str = "runtime host call interrupted";

fn wait_for_host_worker<T>(
    result_rx: mpsc::Receiver<Result<T, String>>,
    handle: std::thread::JoinHandle<()>,
    cancel: tokio::sync::oneshot::Sender<()>,
    runtime_interrupt: &RuntimeInterruptHandle,
    cancellation: &FetchCancellation,
    name: &str,
) -> Result<T, String> {
    let mut cancel = Some(cancel);
    loop {
        if runtime_interrupt.is_terminated() || cancellation.is_cancelled() {
            let _ = cancel
                .take()
                .expect("host worker cancellation is single-use")
                .send(());
            handle
                .join()
                .map_err(|_| format!("{name} worker panicked"))?;
            return Err(HOST_CALL_INTERRUPTED.to_owned());
        }
        match result_rx.recv_timeout(HOST_WORKER_POLL_INTERVAL) {
            Ok(result) => {
                handle
                    .join()
                    .map_err(|_| format!("{name} worker panicked"))?;
                if runtime_interrupt.is_terminated() || cancellation.is_cancelled() {
                    return Err(HOST_CALL_INTERRUPTED.to_owned());
                }
                return result;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                handle
                    .join()
                    .map_err(|_| format!("{name} worker panicked"))?;
                return Err(format!("{name} worker closed without a result"));
            }
        }
    }
}

fn commit_host_effect<T>(
    runtime_interrupt: &RuntimeInterruptHandle,
    cancellation: &FetchCancellation,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if cancellation.is_cancelled() {
        return Err(HOST_CALL_INTERRUPTED.to_owned());
    }
    runtime_interrupt
        .with_active_execution(|| {
            if cancellation.is_cancelled() {
                Err(HOST_CALL_INTERRUPTED.to_owned())
            } else {
                operation()
            }
        })
        .ok_or_else(|| HOST_CALL_INTERRUPTED.to_owned())?
}

fn add_cache_revalidation_headers(headers: &mut Vec<(String, String)>, cached: &TextResponse) {
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("if-none-match"))
        && let Some(etag) = cached.header("etag")
    {
        headers.push(("if-none-match".to_owned(), etag.to_owned()));
    }
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("if-modified-since"))
        && let Some(last_modified) = cached.header("last-modified")
    {
        headers.push(("if-modified-since".to_owned(), last_modified.to_owned()));
    }
}

fn revalidated_response(response: TextResponse, cached: Option<TextResponse>) -> TextResponse {
    if response.status != 304 {
        return response;
    }
    let Some(mut cached) = cached else {
        return response;
    };
    cached.final_url = response.final_url;
    cached.redirects = response.redirects;
    cached.set_cookie = response.set_cookie;
    cached.events = response.events;
    for (name, value) in response.headers {
        cached.headers.insert(name, value);
    }
    cached
}

fn web_storage_store(storage: &WebStorageHost) -> Option<Arc<Store>> {
    match &storage.backend {
        WebStorageBackend::Store(store) => Some(store.clone()),
        WebStorageBackend::Memory(_) => None,
    }
}

fn cookie_origin_key(url: &Url) -> String {
    Origin::from_url(url).partition_key()
}

fn load_cookie_jar(
    shared: &Arc<Mutex<CookieJar>>,
    store: Option<&Store>,
    origin_key: &str,
) -> Result<CookieJar, String> {
    let mut snapshots = Vec::new();
    if let Some(store) = store {
        for record in store
            .cookies_for(origin_key)
            .map_err(|err| err.to_string())?
        {
            snapshots.push(cookie_record_to_snapshot(record));
        }
    }
    snapshots.extend(
        shared
            .lock()
            .map_err(|_| "cookie jar poisoned".to_owned())?
            .snapshots(),
    );
    Ok(CookieJar::from_snapshots(snapshots))
}

pub(super) fn merge_profile_cookies(
    store: &Store,
    url: &Url,
    jar: &mut CookieJar,
    profile_baseline: &mut Vec<CookieSnapshot>,
) -> Result<(), String> {
    let worker_delta = jar.delta_from_snapshots(profile_baseline);
    let origin_key = cookie_origin_key(url);
    let mut snapshots = profile_baseline.clone();
    snapshots.extend(
        store
            .cookies_for(&origin_key)
            .map_err(|err| err.to_string())?
            .into_iter()
            .map(cookie_record_to_snapshot),
    );
    let mut merged = CookieJar::from_snapshots(snapshots);
    *profile_baseline = merged.snapshots();
    merged.apply_delta(worker_delta);
    *jar = merged;
    Ok(())
}

pub(super) fn persist_profile_cookie_delta(
    store: &Store,
    url: &Url,
    delta: &vixen_net::CookieJarDelta,
) -> Result<(), String> {
    let origin_key = cookie_origin_key(url);
    let origin_host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let snapshots = store
        .cookies_for(&origin_key)
        .map_err(|err| err.to_string())?
        .into_iter()
        .map(cookie_record_to_snapshot);
    let mut jar = CookieJar::from_snapshots(snapshots);
    jar.apply_delta(delta.clone());
    persist_cookie_snapshots(store, &origin_key, &origin_host, &jar.snapshots())
}

fn persist_cookie_jar(
    shared: &Arc<Mutex<CookieJar>>,
    store: Option<&Store>,
    origin_key: &str,
    origin_host: &str,
    jar: CookieJar,
) -> Result<(), String> {
    let snapshots = jar.snapshots();
    shared
        .lock()
        .map_err(|_| "cookie jar poisoned".to_owned())?
        .replace_with_snapshots(snapshots.clone());

    let Some(store) = store else {
        return Ok(());
    };
    persist_cookie_snapshots(store, origin_key, origin_host, &snapshots)
}

fn persist_cookie_snapshots(
    store: &Store,
    origin_key: &str,
    origin_host: &str,
    snapshots: &[CookieSnapshot],
) -> Result<(), String> {
    store
        .clear_cookies(origin_key)
        .map_err(|err| err.to_string())?;
    for snapshot in snapshots
        .iter()
        .filter(|snapshot| cookie_snapshot_matches_host(snapshot, origin_host))
    {
        store
            .put_cookie(origin_key, &cookie_snapshot_to_record(snapshot.clone()))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn persist_fetch_cache(
    store: Option<&Store>,
    origin_key: &str,
    response: &TextResponse,
) -> Result<(), String> {
    let Some(store) = store else {
        return Ok(());
    };
    let Some(entry) = crate::http_cache::cache_entry(
        response.status,
        &response.headers,
        response.body.as_bytes().to_vec(),
        &response.request_headers,
        current_unix_timestamp(),
    ) else {
        return Ok(());
    };
    store
        .put_cache(origin_key, &response.final_url, &entry)
        .map_err(|err| err.to_string())
}

fn fetch_cache_lookup(
    store: Option<&Store>,
    url: &Url,
    request_headers: &std::collections::BTreeMap<String, String>,
    max_body_bytes: u64,
) -> Result<Option<(TextResponse, crate::http_cache::CacheUse)>, String> {
    let Some(store) = store else {
        return Ok(None);
    };
    let origin_key = cookie_origin_key(url);
    let Some(entry) = store
        .get_cache(&origin_key, url.as_str())
        .map_err(|err| err.to_string())?
    else {
        return Ok(None);
    };
    let cache_use = crate::http_cache::cache_use(
        &entry,
        request_headers,
        current_unix_timestamp(),
        max_body_bytes,
    );
    if cache_use == crate::http_cache::CacheUse::Unusable {
        return Ok(None);
    }
    Ok(Some((
        TextResponse {
            body: String::from_utf8_lossy(&entry.body).into_owned(),
            headers: entry.headers.into_iter().collect(),
            status: entry.status,
            final_url: url.as_str().to_owned(),
            set_cookie: Vec::new(),
            redirects: 0,
            events: Vec::new(),
            request_headers: request_headers.clone(),
        },
        cache_use,
    )))
}

fn text_response_has_validator(response: &TextResponse) -> bool {
    response.header("etag").is_some() || response.header("last-modified").is_some()
}

fn cached_response_events(url: &Url, status: u16, body_bytes: usize) -> Vec<NetworkEvent> {
    let mut events = vec![
        NetworkEvent::RequestStart {
            url: url.to_string(),
            method: Method::Get,
        },
        NetworkEvent::Response {
            url: url.to_string(),
            status,
        },
    ];
    if body_bytes > 0 {
        events.push(NetworkEvent::BodyProgress {
            url: url.to_string(),
            chunk_bytes: body_bytes as u64,
            loaded_bytes: body_bytes as u64,
            total_bytes: Some(body_bytes as u64),
        });
    }
    events.push(NetworkEvent::Completed {
        url: url.to_string(),
        body_bytes: body_bytes as u64,
    });
    events
}

fn cookie_record_to_snapshot(record: CookieRecord) -> CookieSnapshot {
    CookieSnapshot {
        name: record.name,
        value: record.value,
        domain: record.domain,
        host_only: record.host_only,
        path: record.path,
        expires_unix: record.expires_unix,
        secure: record.secure,
        http_only: record.http_only,
        same_site: same_site_from_store(record.same_site),
        creation_unix: record.creation_unix,
    }
}

fn cookie_snapshot_to_record(snapshot: CookieSnapshot) -> CookieRecord {
    CookieRecord {
        name: snapshot.name,
        value: snapshot.value,
        domain: snapshot.domain,
        host_only: snapshot.host_only,
        path: snapshot.path,
        expires_unix: snapshot.expires_unix,
        secure: snapshot.secure,
        http_only: snapshot.http_only,
        same_site: same_site_to_store(snapshot.same_site),
        creation_unix: snapshot.creation_unix,
    }
}

fn same_site_from_store(value: u8) -> SameSite {
    match value {
        0 => SameSite::Strict,
        2 => SameSite::None,
        _ => SameSite::Lax,
    }
}

fn same_site_to_store(value: SameSite) -> u8 {
    match value {
        SameSite::Strict => 0,
        SameSite::Lax => 1,
        SameSite::None => 2,
    }
}

fn cookie_snapshot_matches_host(snapshot: &CookieSnapshot, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let domain = snapshot.domain.to_ascii_lowercase();
    host == domain || (!snapshot.host_only && host.ends_with(&format!(".{domain}")))
}

fn current_unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

fn storage_partition_key(host: &WebApiHost, kind: StorageKind) -> &str {
    match kind {
        StorageKind::Local => &host.storage.local_partition_key,
        StorageKind::Session => &host.storage.session_partition_key,
    }
}

fn storage_entries(host: &WebApiHost, kind: StorageKind) -> Result<Vec<(String, String)>, String> {
    let partition = storage_partition_key(host, kind);
    match &host.storage.backend {
        WebStorageBackend::Memory(map) => Ok(map
            .lock()
            .map_err(|_| "storage map poisoned".to_owned())?
            .get(partition)
            .cloned()
            .unwrap_or_default()),
        WebStorageBackend::Store(store) => store
            .storage_entries(partition)
            .map_err(|err| err.to_string()),
    }
}

fn storage_get_item(
    host: &WebApiHost,
    kind: StorageKind,
    key: &str,
) -> Result<Option<String>, String> {
    let partition = storage_partition_key(host, kind);
    match &host.storage.backend {
        WebStorageBackend::Memory(map) => Ok(map
            .lock()
            .map_err(|_| "storage map poisoned".to_owned())?
            .get(partition)
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(entry_key, _)| entry_key == key)
                    .map(|(_, value)| value.clone())
            })),
        WebStorageBackend::Store(store) => store
            .get_storage_item(partition, key)
            .map_err(|err| err.to_string()),
    }
}

fn storage_set_item(
    host: &WebApiHost,
    kind: StorageKind,
    key: String,
    value: String,
) -> Result<(), String> {
    let partition = storage_partition_key(host, kind);
    match &host.storage.backend {
        WebStorageBackend::Memory(map) => {
            let mut map = map.lock().map_err(|_| "storage map poisoned".to_owned())?;
            let entries = map.entry(partition.to_owned()).or_default();
            if let Some((_, existing)) = entries.iter_mut().find(|(entry_key, _)| entry_key == &key)
            {
                *existing = value;
            } else {
                entries.push((key, value));
            }
            Ok(())
        }
        WebStorageBackend::Store(store) => store
            .put_storage_item(partition, &key, &value)
            .map_err(|err| err.to_string()),
    }
}

fn storage_remove_item(host: &WebApiHost, kind: StorageKind, key: &str) -> Result<(), String> {
    let partition = storage_partition_key(host, kind);
    match &host.storage.backend {
        WebStorageBackend::Memory(map) => {
            if let Some(entries) = map
                .lock()
                .map_err(|_| "storage map poisoned".to_owned())?
                .get_mut(partition)
            {
                entries.retain(|(entry_key, _)| entry_key != key);
            }
            Ok(())
        }
        WebStorageBackend::Store(store) => store
            .remove_storage_item(partition, key)
            .map_err(|err| err.to_string()),
    }
}

fn storage_clear(host: &WebApiHost, kind: StorageKind) -> Result<(), String> {
    let partition = storage_partition_key(host, kind);
    match &host.storage.backend {
        WebStorageBackend::Memory(map) => {
            map.lock()
                .map_err(|_| "storage map poisoned".to_owned())?
                .remove(partition);
            Ok(())
        }
        WebStorageBackend::Store(store) => store
            .clear_storage_partition(partition)
            .map_err(|err| err.to_string()),
    }
}

fn canonical_permission_name(name: &str) -> Option<&'static str> {
    match name {
        "geolocation" => Some("geolocation"),
        "notifications" => Some("notifications"),
        "camera" => Some("camera"),
        "microphone" => Some("microphone"),
        "clipboard-read" => Some("clipboard-read"),
        "persistent-storage" => Some("persistent-storage"),
        _ => None,
    }
}

fn permission_state(host: &WebApiHost, kind: &'static str) -> Result<&'static str, String> {
    let origin = host
        .fetch_policy
        .as_ref()
        .map(|policy| policy.cors_origin());
    if let Some(granted) = host
        .permission_overrides
        .decision(origin.as_deref(), kind)?
    {
        return Ok(if granted { "granted" } else { "denied" });
    }
    let Some(policy) = host.fetch_policy.as_ref() else {
        return Ok("prompt");
    };
    let WebStorageBackend::Store(store) = &host.storage.backend else {
        return Ok("prompt");
    };
    let Some(record) = store
        .permission(&policy.origin.partition_key(), kind)
        .map_err(|err| format!("permission store read failed: {err}"))?
    else {
        return Ok("prompt");
    };
    Ok(match record.decision {
        PermissionDecision::Granted => "granted",
        PermissionDecision::Denied => "denied",
    })
}

fn storage_check_quota(
    entries: &[(String, String)],
    key: &str,
    value: &str,
) -> Result<(), StorageKeyError> {
    let current_bytes = storage_total_bytes(entries);
    let Some((old_key, old_value)) = entries.iter().find(|(entry_key, _)| entry_key == key) else {
        return StorageQuota {
            entries: entries.len(),
            bytes: current_bytes,
        }
        .check(key.len(), value.len());
    };

    let old_bytes = old_key.len() + old_value.len();
    let projected_bytes = current_bytes - old_bytes + key.len() + value.len();
    if projected_bytes > MAX_PARTITION_BYTES {
        return Err(StorageKeyError::TooLong {
            what: "partition-bytes",
            len: projected_bytes,
            max: MAX_PARTITION_BYTES,
        });
    }
    Ok(())
}

fn storage_total_bytes(entries: &[(String, String)]) -> usize {
    entries
        .iter()
        .map(|(key, value)| key.len() + value.len())
        .sum()
}

fn storage_value(value: Option<String>) -> deno_core::serde_json::Value {
    json!({
        "ok": true,
        "value": value,
    })
}

fn apply_fetch_integrity(response: TextResponse, metadata: &str) -> Result<TextResponse, String> {
    if metadata.is_empty() {
        return Ok(response);
    }
    let items = parse_integrity(metadata);
    match verify_integrity(&items, response.body.as_bytes()) {
        IntegrityOutcome::Mismatch(algorithms) => Err(format!(
            "fetch blocked by integrity mismatch ({})",
            algorithms
                .iter()
                .map(|algorithm| algorithm.token())
                .collect::<Vec<_>>()
                .join(",")
        )),
        IntegrityOutcome::NoMetadata
        | IntegrityOutcome::NoKnownAlgorithms
        | IntegrityOutcome::Verified(_) => Ok(response),
    }
}

fn apply_fetch_visibility(
    mut response: TextResponse,
    fetch_mode: FetchMode,
    credentials_mode: FetchCredentialsMode,
    fetch_policy: Option<&FetchPolicy>,
    url: &Url,
) -> Result<(TextResponse, &'static str), String> {
    let Some(policy) = fetch_policy else {
        return Ok((response, "basic"));
    };
    let visibility_url = Url::parse(&response.final_url).unwrap_or_else(|_| url.clone());
    if !policy.is_cross_origin(&visibility_url) {
        return Ok((response, "basic"));
    }
    match fetch_mode {
        FetchMode::SameOrigin => Err("fetch blocked by mode same-origin".to_owned()),
        FetchMode::NoCors => {
            response.body.clear();
            response.headers.clear();
            response.status = 0;
            response.final_url.clear();
            response.set_cookie.clear();
            Ok((response, "opaque"))
        }
        FetchMode::Cors => {
            let cors_headers = CorsResponseHeaders::from_headers(
                response
                    .headers
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
            match cors_check(
                &cors_headers,
                &policy.cors_origin(),
                credentials_mode.cors_mode(),
            ) {
                CorsCheckOutcome::Pass => {
                    response.headers = cors_filtered_headers(
                        response
                            .headers
                            .iter()
                            .map(|(name, value)| (name.as_str(), value.as_str())),
                        &cors_headers.expose_headers,
                    )
                    .into_iter()
                    .map(|(name, value)| (name.to_ascii_lowercase(), value))
                    .collect();
                    Ok((response, "cors"))
                }
                CorsCheckOutcome::Fail(err) => Err(format!("fetch blocked by CORS: {err}")),
            }
        }
    }
}

fn fetch_response(response: TextResponse, response_type: &str) -> deno_core::serde_json::Value {
    let body_chunks = response
        .events
        .iter()
        .filter_map(|event| match event {
            NetworkEvent::BodyProgress { chunk_bytes, .. } => Some(*chunk_bytes),
            _ => None,
        })
        .collect::<Vec<_>>();
    json!({
        "ok": true,
        "body": response.body,
        "headers": response.headers,
        "status": response.status,
        "finalUrl": response.final_url,
        "redirected": response.redirects > 0,
        "responseType": response_type,
        "bodyChunks": body_chunks,
        "events": response.events.into_iter().map(network_event_value).collect::<Vec<_>>(),
    })
}

fn network_event_value(event: NetworkEvent) -> deno_core::serde_json::Value {
    match event {
        NetworkEvent::RequestStart { url, method } => json!({
            "type": "request",
            "url": url,
            "method": method.as_str(),
        }),
        NetworkEvent::Redirect { from, to, status } => json!({
            "type": "redirect",
            "from": from,
            "to": to,
            "status": status,
        }),
        NetworkEvent::Response { url, status } => json!({
            "type": "response",
            "url": url,
            "status": status,
        }),
        NetworkEvent::BodyProgress {
            url,
            chunk_bytes,
            loaded_bytes,
            total_bytes,
        } => json!({
            "type": "progress",
            "url": url,
            "chunkBytes": chunk_bytes,
            "loadedBytes": loaded_bytes,
            "totalBytes": total_bytes,
        }),
        NetworkEvent::Completed { url, body_bytes } => json!({
            "type": "completed",
            "url": url,
            "bodyBytes": body_bytes,
        }),
    }
}

fn fetch_failure(
    url: &str,
    method: Method,
    message: impl Into<String>,
    blocked_reason: &'static str,
) -> deno_core::serde_json::Value {
    let message = message.into();
    json!({
        "ok": false,
        "message": message,
        "events": [
            {
                "type": "request",
                "url": url,
                "method": method.as_str(),
            },
            {
                "type": "failure",
                "url": url,
                "errorText": message,
                "blockedReason": blocked_reason,
            },
        ],
    })
}

fn fetch_blocked_reason(message: &str) -> &'static str {
    if message.contains("integrity") {
        "integrity"
    } else if message.contains("CORS") || message.contains("cors") {
        "cors"
    } else if message.contains("same-origin") {
        "origin"
    } else {
        "network"
    }
}

fn fetch_error(message: impl Into<String>) -> deno_core::serde_json::Value {
    json!({
        "ok": false,
        "message": message.into(),
    })
}

fn storage_error(message: impl Into<String>) -> deno_core::serde_json::Value {
    json!({
        "ok": false,
        "message": message.into(),
    })
}

fn storage_key_error(err: StorageKeyError) -> deno_core::serde_json::Value {
    storage_error(err.to_string())
}

fn storage_quota_error(err: StorageKeyError) -> deno_core::serde_json::Value {
    json!({
        "ok": false,
        "name": "QuotaExceededError",
        "message": err.to_string(),
    })
}

const WEB_API_BOOTSTRAP: &str = r#"
(() => {
  const {
    op_vixen_storage_length,
    op_vixen_storage_key,
    op_vixen_storage_get,
    op_vixen_storage_set,
    op_vixen_storage_remove,
    op_vixen_storage_clear,
    op_vixen_storage_estimate,
    op_vixen_storage_persisted,
    op_vixen_permission_query,
    op_vixen_crypto_random_bytes,
    op_vixen_fetch_start,
    op_vixen_fetch_finish,
    op_vixen_fetch_cancel,
  } = Deno.core.ops;
  const webidl = globalThis.__vixenWebidl;
  const textEncoder = typeof TextEncoder === 'function' ? new TextEncoder() : null;
  const textDecoder = typeof TextDecoder === 'function' ? new TextDecoder() : null;
  const startEpoch = Date.now();

  function defineGlobal(name, value) {
    Object.defineProperty(globalThis, name, {
      value,
      writable: true,
      configurable: true,
    });
  }

  function defineReadonly(target, name, value, enumerable = true) {
    Object.defineProperty(target, name, {
      value,
      enumerable,
      configurable: true,
    });
  }

  function defineData(target, name, value, enumerable = true) {
    Object.defineProperty(target, name, {
      value,
      writable: true,
      enumerable,
      configurable: true,
    });
  }

  function syncIndexedValues(target, values) {
    const previous = Number(target.__vixenIndexedLength) || 0;
    for (let i = 0; i < previous; i++) delete target[String(i)];
    for (let i = 0; i < values.length; i++) {
      Object.defineProperty(target, String(i), {
        value: values[i],
        enumerable: true,
        configurable: true,
      });
    }
    defineData(target, '__vixenIndexedLength', values.length, false);
  }

  // -----------------------------------------------------------------------
  // Console capture
  // -----------------------------------------------------------------------

  const consoleEvents = [];

  function consoleArg(value) {
    if (value === null) {
      return { type: 'object', subtype: 'null', value: null, description: 'null' };
    }
    const type = typeof value;
    if (type === 'undefined') {
      return { type: 'undefined', description: 'undefined' };
    }
    if (type === 'string') {
      return { type: 'string', value, description: value };
    }
    if (type === 'number') {
      return { type: 'number', value, description: String(value) };
    }
    if (type === 'boolean') {
      return { type: 'boolean', value, description: String(value) };
    }
    if (type === 'bigint') {
      return { type: 'bigint', unserializableValue: String(value) + 'n', description: String(value) + 'n' };
    }
    try {
      return { type: 'object', description: String(value) };
    } catch (_) {
      return { type: 'object', description: '[object]' };
    }
  }

  function recordConsole(type, args) {
    consoleEvents.push({ type, args: Array.prototype.map.call(args, consoleArg) });
  }

  const vixenConsole = Object.assign({}, globalThis.console || {});
  for (const type of ['log', 'debug', 'info', 'warn', 'error']) {
    Object.defineProperty(vixenConsole, type, {
      value: function (...args) { recordConsole(type, args); },
      writable: true,
      configurable: true,
    });
  }
  defineGlobal('console', vixenConsole);
  defineGlobal('__vixenDrainConsoleEvents', function () {
    return consoleEvents.splice(0, consoleEvents.length);
  });

  // -----------------------------------------------------------------------
  // Network event capture
  // -----------------------------------------------------------------------

  const networkEvents = [];
  let networkRequestSequence = 0;

  function recordNetworkEvents(events) {
    if (!Array.isArray(events) || events.length === 0) return;
    const requestId = 'fetch-' + (++networkRequestSequence);
    for (const event of events) {
      if (!event || typeof event !== 'object') continue;
      networkEvents.push(Object.assign({ requestId }, event));
    }
  }

  defineGlobal('__vixenDrainNetworkEvents', function () {
    return networkEvents.splice(0, networkEvents.length);
  });

  // -----------------------------------------------------------------------
  // Modal dialogs
  // -----------------------------------------------------------------------

  const dialogEvents = [];

  function recordDialog(type, message, defaultPrompt) {
    dialogEvents.push({ type, message: String(message), defaultPrompt: String(defaultPrompt ?? '') });
  }

  defineGlobal('alert', function (message = '') {
    recordDialog('alert', message, '');
  });
  defineGlobal('confirm', function (message = '') {
    recordDialog('confirm', message, '');
    return true;
  });
  defineGlobal('prompt', function (message = '', defaultValue = '') {
    recordDialog('prompt', message, defaultValue);
    return String(defaultValue ?? '');
  });
  defineGlobal('__vixenDrainDialogEvents', function () {
    return dialogEvents.splice(0, dialogEvents.length);
  });

  function copyPrototypeMembers(source, target) {
    for (const name of Reflect.ownKeys(source)) {
      if (name === 'constructor') continue;
      if (Object.prototype.hasOwnProperty.call(target, name)) continue;
      Object.defineProperty(target, name, Object.getOwnPropertyDescriptor(source, name));
    }
  }

  function finiteNumber(value, fallback = 0) {
    const number = Number(value);
    return Number.isFinite(number) ? number : fallback;
  }

  function byteLength(value) {
    const string = String(value);
    return textEncoder ? textEncoder.encode(string).length : string.length;
  }

  function bytesFromString(value) {
    const string = String(value);
    if (textEncoder) return textEncoder.encode(string);
    const bytes = new Uint8Array(string.length);
    for (let i = 0; i < string.length; i++) bytes[i] = string.charCodeAt(i) & 0xff;
    return bytes;
  }

  function bytesFromPart(part) {
    if (part instanceof VixenBlob) return part.__vixenBytes.slice();
    if (part instanceof ArrayBuffer) return new Uint8Array(part).slice();
    if (ArrayBuffer.isView && ArrayBuffer.isView(part)) {
      return new Uint8Array(part.buffer, part.byteOffset, part.byteLength).slice();
    }
    return bytesFromString(part);
  }

  function concatBytes(parts) {
    const total = parts.reduce((sum, part) => sum + part.length, 0);
    const out = new Uint8Array(total);
    let offset = 0;
    for (const part of parts) {
      out.set(part, offset);
      offset += part.length;
    }
    return out;
  }

  function textFromBytes(bytes) {
    if (textDecoder) return textDecoder.decode(bytes);
    let out = '';
    for (const byte of bytes) out += String.fromCharCode(byte);
    return out;
  }

  // -----------------------------------------------------------------------
  // Event / EventTarget
  // -----------------------------------------------------------------------

  const listeners = new WeakMap();
  const eventState = new WeakMap();

  const CAPTURING_PHASE = 1;
  const AT_TARGET = 2;
  const BUBBLING_PHASE = 3;

  function listenerOptions(options) {
    if (options === undefined || options === null) return { capture: false, once: false, passive: false };
    if (typeof options === 'boolean') return { capture: Boolean(options), once: false, passive: false };
    return {
      capture: Boolean(options.capture),
      once: Boolean(options.once),
      passive: Boolean(options.passive),
    };
  }

  function listenerList(target, type, create) {
    let byType = listeners.get(target);
    if (!byType && create) {
      byType = new Map();
      listeners.set(target, byType);
    }
    if (!byType) return undefined;
    let list = byType.get(type);
    if (!list && create) {
      list = [];
      byType.set(type, list);
    }
    return list;
  }

  function invokeEventListeners(target, event, phase, capture) {
    const state = eventState.get(event);
    if (state.stopped) return;
    state.currentTarget = target;
    state.eventPhase = phase;
    const list = listenerList(target, state.type, false) || [];
    for (const entry of list.slice()) {
      if (state.immediateStopped) break;
      if (entry.capture !== capture && phase !== AT_TARGET) continue;
      if (phase === AT_TARGET && entry.capture !== capture) continue;
      if (!list.includes(entry)) continue;
      if (entry.passive) state.inPassiveListener = true;
      if (typeof entry.callback === 'function') {
        entry.callback.call(target, event);
      } else if (entry.callback && typeof entry.callback.handleEvent === 'function') {
        entry.callback.handleEvent(event);
      }
      state.inPassiveListener = false;
      if (entry.once) {
        const index = list.indexOf(entry);
        if (index >= 0) list.splice(index, 1);
      }
    }
    state.currentTarget = null;
  }

  function invokeEventHandlerAttribute(target, event) {
    const handler = target && target['on' + event.type];
    if (typeof handler === 'function') handler.call(target, event);
  }

  function eventPathFor(target, event) {
    const hook = globalThis.__vixenEventPathForTarget;
    if (typeof hook === 'function') {
      const path = hook(target, event);
      if (Array.isArray(path) && path.length > 0) return path;
    }
    return [target];
  }

  function runDefaultAction(target, event) {
    const hook = globalThis.__vixenRunDefaultAction;
    if (typeof hook !== 'function') return;
    const state = eventState.get(event);
    if (!state || state.defaultPrevented) return;
    hook(target, event);
  }

  class VixenEventTarget {
    addEventListener(type, callback, options = undefined) {
      if (callback === null || callback === undefined) return;
      const eventType = String(type);
      const parsed = listenerOptions(options);
      const list = listenerList(this, eventType, true);
      if (!list.some((entry) => entry.callback === callback && entry.capture === parsed.capture)) {
        list.push({ callback, capture: parsed.capture, once: parsed.once, passive: parsed.passive });
      }
    }
    removeEventListener(type, callback, options = undefined) {
      const list = listenerList(this, String(type), false);
      if (!list) return;
      const capture = listenerOptions(options).capture;
      const index = list.findIndex((entry) => entry.callback === callback && entry.capture === capture);
      if (index >= 0) list.splice(index, 1);
    }
    dispatchEvent(event) {
      const state = eventState.get(event);
      if (!state) throw new TypeError('dispatchEvent expects an Event');
      if (state.dispatching) throw new TypeError('Event is already being dispatched');
      state.dispatching = true;
      state.stopped = false;
      state.immediateStopped = false;
      state.target = this;
      const path = eventPathFor(this, event);
      state.path = path.slice();

      for (let i = path.length - 1; i >= 1; i--) {
        invokeEventListeners(path[i], event, CAPTURING_PHASE, true);
        if (state.stopped) break;
      }
      if (!state.stopped) invokeEventListeners(this, event, AT_TARGET, true);
      if (!state.immediateStopped) invokeEventListeners(this, event, AT_TARGET, false);
      if (!state.immediateStopped) invokeEventHandlerAttribute(this, event);
      if (state.bubbles && !state.stopped) {
        for (let i = 1; i < path.length; i++) {
          invokeEventListeners(path[i], event, BUBBLING_PHASE, false);
          if (!state.immediateStopped) invokeEventHandlerAttribute(path[i], event);
          if (state.stopped) break;
        }
      }
      state.dispatching = false;
      state.currentTarget = null;
      state.eventPhase = 0;
      runDefaultAction(this, event);
      return !(state.cancelable && state.defaultPrevented);
    }
  }

  class VixenEvent {
    constructor(type, init = {}) {
      eventState.set(this, {
        type: String(type),
        bubbles: Boolean(init && init.bubbles),
        cancelable: Boolean(init && init.cancelable),
        composed: Boolean(init && init.composed),
        defaultPrevented: false,
        stopped: false,
        immediateStopped: false,
        inPassiveListener: false,
        dispatching: false,
        target: null,
        currentTarget: null,
        eventPhase: 0,
        timeStamp: performance.now(),
        path: [],
      });
    }
    get type() { return eventState.get(this).type; }
    get target() { return eventState.get(this).target; }
    get currentTarget() { return eventState.get(this).currentTarget; }
    get eventPhase() { return eventState.get(this).eventPhase; }
    get bubbles() { return eventState.get(this).bubbles; }
    get cancelable() { return eventState.get(this).cancelable; }
    get defaultPrevented() { return eventState.get(this).defaultPrevented; }
    get composed() { return eventState.get(this).composed; }
    get timeStamp() { return eventState.get(this).timeStamp; }
    get isTrusted() { return false; }
    stopPropagation() { eventState.get(this).stopped = true; }
    stopImmediatePropagation() {
      const state = eventState.get(this);
      state.stopped = true;
      state.immediateStopped = true;
    }
    preventDefault() {
      const state = eventState.get(this);
      if (state.cancelable && !state.inPassiveListener) state.defaultPrevented = true;
    }
    composedPath() { return eventState.get(this).path.slice(); }
  }

  for (const [name, value] of [['NONE', 0], ['CAPTURING_PHASE', CAPTURING_PHASE], ['AT_TARGET', AT_TARGET], ['BUBBLING_PHASE', BUBBLING_PHASE]]) {
    defineReadonly(VixenEvent, name, value, false);
    defineReadonly(VixenEvent.prototype, name, value, false);
  }

  class VixenCustomEvent extends VixenEvent {
    constructor(type, init = {}) {
      super(type, init);
      defineReadonly(this, '__vixenDetail', init && Object.prototype.hasOwnProperty.call(init, 'detail') ? init.detail : null, false);
    }
    get detail() { return this.__vixenDetail; }
    initCustomEvent() {}
  }

  class VixenUIEvent extends VixenEvent {
    constructor(type, init = {}) {
      super(type, init);
      defineReadonly(this, 'view', init && Object.prototype.hasOwnProperty.call(init, 'view') ? init.view : globalThis, false);
      defineReadonly(this, 'detail', Number(init && init.detail) || 0, false);
    }
  }

  class VixenMouseEvent extends VixenUIEvent {
    constructor(type, init = {}) {
      super(type, init);
      const opts = init || {};
      defineReadonly(this, 'screenX', Number(opts.screenX) || 0, false);
      defineReadonly(this, 'screenY', Number(opts.screenY) || 0, false);
      defineReadonly(this, 'clientX', Number(opts.clientX) || 0, false);
      defineReadonly(this, 'clientY', Number(opts.clientY) || 0, false);
      defineReadonly(this, 'ctrlKey', Boolean(opts.ctrlKey), false);
      defineReadonly(this, 'shiftKey', Boolean(opts.shiftKey), false);
      defineReadonly(this, 'altKey', Boolean(opts.altKey), false);
      defineReadonly(this, 'metaKey', Boolean(opts.metaKey), false);
      defineReadonly(this, 'button', Number(opts.button) || 0, false);
      defineReadonly(this, 'buttons', Number(opts.buttons) || 0, false);
      defineReadonly(this, 'relatedTarget', opts.relatedTarget || null, false);
    }
    getModifierState(key) {
      switch (String(key)) {
        case 'Control': return this.ctrlKey;
        case 'Shift': return this.shiftKey;
        case 'Alt': return this.altKey;
        case 'Meta': return this.metaKey;
        default: return false;
      }
    }
  }

  class VixenFocusEvent extends VixenUIEvent {
    constructor(type, init = {}) {
      super(type, init);
      const opts = init || {};
      defineReadonly(this, 'relatedTarget', opts.relatedTarget || null, false);
    }
  }

  class VixenSubmitEvent extends VixenEvent {
    constructor(type, init = {}) {
      super(type, init);
      const opts = init || {};
      defineReadonly(this, 'submitter', opts.submitter || null, false);
    }
  }

  class VixenWheelEvent extends VixenMouseEvent {
    constructor(type, init = {}) {
      super(type, init);
      const opts = init || {};
      defineReadonly(this, 'deltaX', Number(opts.deltaX) || 0, false);
      defineReadonly(this, 'deltaY', Number(opts.deltaY) || 0, false);
      defineReadonly(this, 'deltaZ', Number(opts.deltaZ) || 0, false);
      defineReadonly(this, 'deltaMode', Number(opts.deltaMode) || 0, false);
    }
  }

  for (const [name, value] of [['DOM_DELTA_PIXEL', 0], ['DOM_DELTA_LINE', 1], ['DOM_DELTA_PAGE', 2]]) {
    defineReadonly(VixenWheelEvent, name, value, false);
    defineReadonly(VixenWheelEvent.prototype, name, value, false);
  }

  class VixenKeyboardEvent extends VixenUIEvent {
    constructor(type, init = {}) {
      super(type, init);
      const opts = init || {};
      defineReadonly(this, 'key', opts.key === undefined ? '' : String(opts.key), false);
      defineReadonly(this, 'code', opts.code === undefined ? '' : String(opts.code), false);
      defineReadonly(this, 'location', Number(opts.location) || 0, false);
      defineReadonly(this, 'ctrlKey', Boolean(opts.ctrlKey), false);
      defineReadonly(this, 'shiftKey', Boolean(opts.shiftKey), false);
      defineReadonly(this, 'altKey', Boolean(opts.altKey), false);
      defineReadonly(this, 'metaKey', Boolean(opts.metaKey), false);
      defineReadonly(this, 'repeat', Boolean(opts.repeat), false);
      defineReadonly(this, 'isComposing', Boolean(opts.isComposing), false);
    }
    getModifierState(key) {
      switch (String(key)) {
        case 'Control': return this.ctrlKey;
        case 'Shift': return this.shiftKey;
        case 'Alt': return this.altKey;
        case 'Meta': return this.metaKey;
        default: return false;
      }
    }
  }

  class VixenInputEvent extends VixenUIEvent {
    constructor(type, init = {}) {
      super(type, init);
      const opts = init || {};
      defineReadonly(this, 'data', opts.data === undefined ? null : (opts.data === null ? null : String(opts.data)), false);
      defineReadonly(this, 'isComposing', Boolean(opts.isComposing), false);
      defineReadonly(this, 'inputType', opts.inputType === undefined ? '' : String(opts.inputType), false);
      defineReadonly(this, 'dataTransfer', opts.dataTransfer || null, false);
    }
    getTargetRanges() { return []; }
  }

  webidl.adoptInterface('EventTarget', VixenEventTarget);
  webidl.adoptInterface('Event', VixenEvent);
  webidl.adoptInterface('CustomEvent', VixenCustomEvent);
  webidl.adoptInterface('UIEvent', VixenUIEvent);
  webidl.adoptInterface('MouseEvent', VixenMouseEvent);
  webidl.adoptInterface('FocusEvent', VixenFocusEvent);
  webidl.adoptInterface('SubmitEvent', VixenSubmitEvent);
  webidl.adoptInterface('WheelEvent', VixenWheelEvent);
  webidl.adoptInterface('KeyboardEvent', VixenKeyboardEvent);
  webidl.adoptInterface('InputEvent', VixenInputEvent);
  for (const name of ['addEventListener', 'removeEventListener', 'dispatchEvent']) {
    if (typeof globalThis[name] !== 'function') {
      Object.defineProperty(globalThis, name, {
        value: VixenEventTarget.prototype[name],
        writable: true,
        configurable: true,
      });
    }
  }

  // -----------------------------------------------------------------------
  // Geometry Interfaces
  // -----------------------------------------------------------------------

  function pointInit(init) {
    init = init || {};
    return {
      x: finiteNumber(init.x, 0),
      y: finiteNumber(init.y, 0),
      z: finiteNumber(init.z, 0),
      w: init.w === undefined ? 1 : finiteNumber(init.w, 1),
    };
  }

  class VixenDOMPointReadOnly {
    constructor(x = 0, y = 0, z = 0, w = 1) {
      defineData(this, 'x', finiteNumber(x, 0));
      defineData(this, 'y', finiteNumber(y, 0));
      defineData(this, 'z', finiteNumber(z, 0));
      defineData(this, 'w', finiteNumber(w, 1));
    }
    matrixTransform(matrix = new VixenDOMMatrix()) {
      return new VixenDOMMatrix(matrix).transformPoint(this);
    }
    toJSON() { return { x: this.x, y: this.y, z: this.z, w: this.w }; }
  }

  class VixenDOMPoint extends VixenDOMPointReadOnly {
    static fromPoint(init = {}) {
      const point = pointInit(init);
      return new VixenDOMPoint(point.x, point.y, point.z, point.w);
    }
  }

  function rectInit(init) {
    init = init || {};
    return {
      x: finiteNumber(init.x, 0),
      y: finiteNumber(init.y, 0),
      width: finiteNumber(init.width, 0),
      height: finiteNumber(init.height, 0),
    };
  }

  class VixenDOMRectReadOnly {
    constructor(x = 0, y = 0, width = 0, height = 0) {
      defineData(this, 'x', finiteNumber(x, 0));
      defineData(this, 'y', finiteNumber(y, 0));
      defineData(this, 'width', finiteNumber(width, 0));
      defineData(this, 'height', finiteNumber(height, 0));
    }
    get left() { return Math.min(this.x, this.x + this.width); }
    get top() { return Math.min(this.y, this.y + this.height); }
    get right() { return Math.max(this.x, this.x + this.width); }
    get bottom() { return Math.max(this.y, this.y + this.height); }
    toJSON() {
      return {
        x: this.x,
        y: this.y,
        width: this.width,
        height: this.height,
        top: this.top,
        right: this.right,
        bottom: this.bottom,
        left: this.left,
      };
    }
    static fromRect(init = {}) {
      const rect = rectInit(init);
      return new VixenDOMRectReadOnly(rect.x, rect.y, rect.width, rect.height);
    }
  }

  class VixenDOMRect extends VixenDOMRectReadOnly {
    static fromRect(init = {}) {
      const rect = rectInit(init);
      return new VixenDOMRect(rect.x, rect.y, rect.width, rect.height);
    }
  }

  class VixenDOMQuad {
    constructor(p1 = {}, p2 = {}, p3 = {}, p4 = {}) {
      const a = pointInit(p1), b = pointInit(p2), c = pointInit(p3), d = pointInit(p4);
      defineData(this, 'p1', new VixenDOMPoint(a.x, a.y, a.z, a.w));
      defineData(this, 'p2', new VixenDOMPoint(b.x, b.y, b.z, b.w));
      defineData(this, 'p3', new VixenDOMPoint(c.x, c.y, c.z, c.w));
      defineData(this, 'p4', new VixenDOMPoint(d.x, d.y, d.z, d.w));
    }
    static fromRect(init = {}) {
      const r = VixenDOMRect.fromRect(init);
      return new VixenDOMQuad(
        { x: r.left, y: r.top },
        { x: r.right, y: r.top },
        { x: r.right, y: r.bottom },
        { x: r.left, y: r.bottom },
      );
    }
    static fromQuad(init = {}) {
      return new VixenDOMQuad(init.p1, init.p2, init.p3, init.p4);
    }
    getBounds() {
      const xs = [this.p1.x, this.p2.x, this.p3.x, this.p4.x];
      const ys = [this.p1.y, this.p2.y, this.p3.y, this.p4.y];
      const left = Math.min(...xs), right = Math.max(...xs);
      const top = Math.min(...ys), bottom = Math.max(...ys);
      return new VixenDOMRect(left, top, right - left, bottom - top);
    }
    toJSON() { return { p1: this.p1.toJSON(), p2: this.p2.toJSON(), p3: this.p3.toJSON(), p4: this.p4.toJSON() }; }
  }

  function identityMatrix() {
    return [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  }

  function matrixFromInit(init) {
    if (init === undefined || init === null) return identityMatrix();
    if (init instanceof VixenDOMMatrixReadOnly) return init.__vixenMatrix.slice();
    if (ArrayBuffer.isView(init) || Array.isArray(init)) {
      const values = Array.from(init, (value) => finiteNumber(value, 0));
      if (values.length === 6) {
        return [values[0], values[1], 0, 0, values[2], values[3], 0, 0, 0, 0, 1, 0, values[4], values[5], 0, 1];
      }
      if (values.length === 16) return values.slice();
      throw new TypeError('DOMMatrix sequence must have 6 or 16 numbers');
    }
    if (typeof init === 'object') {
      const m = identityMatrix();
      const names = ['m11','m12','m13','m14','m21','m22','m23','m24','m31','m32','m33','m34','m41','m42','m43','m44'];
      for (let i = 0; i < names.length; i++) if (init[names[i]] !== undefined) m[i] = finiteNumber(init[names[i]], m[i]);
      for (const [alias, index] of [['a',0],['b',1],['c',4],['d',5],['e',12],['f',13]]) {
        if (init[alias] !== undefined) m[index] = finiteNumber(init[alias], m[index]);
      }
      return m;
    }
    throw new TypeError('unsupported DOMMatrix init');
  }

  function multiplyMatrix(a, b) {
    const out = new Array(16).fill(0);
    for (let col = 0; col < 4; col++) {
      for (let row = 0; row < 4; row++) {
        let sum = 0;
        for (let k = 0; k < 4; k++) sum += a[k * 4 + row] * b[col * 4 + k];
        out[col * 4 + row] = sum;
      }
    }
    return out;
  }

  function minor3(m, dropRow, dropCol) {
    const values = [];
    for (let col = 0; col < 4; col++) {
      if (col === dropCol) continue;
      for (let row = 0; row < 4; row++) {
        if (row === dropRow) continue;
        values.push(m[col * 4 + row]);
      }
    }
    return values[0] * (values[4] * values[8] - values[5] * values[7]) -
      values[3] * (values[1] * values[8] - values[2] * values[7]) +
      values[6] * (values[1] * values[5] - values[2] * values[4]);
  }

  function determinantMatrix(m) {
    let det = 0;
    for (let col = 0; col < 4; col++) {
      det += (col % 2 === 0 ? 1 : -1) * m[col * 4] * minor3(m, 0, col);
    }
    return det;
  }

  function inverseMatrix(m) {
    const det = determinantMatrix(m);
    if (det === 0 || !Number.isFinite(det)) return new Array(16).fill(NaN);
    const out = new Array(16).fill(0);
    for (let row = 0; row < 4; row++) {
      for (let col = 0; col < 4; col++) {
        const sign = (row + col) % 2 === 0 ? 1 : -1;
        out[col * 4 + row] = sign * minor3(m, col, row) / det;
      }
    }
    return out;
  }

  function parseMatrixNumberList(input, expected, label) {
    const parts = input.includes(',') ? input.split(',') : input.trim().split(/\s+/);
    if (parts.length !== expected || parts.some((part) => part.trim() === '')) {
      throw new TypeError(label + ' must contain ' + expected + ' numbers');
    }
    const values = parts.map((part) => Number(part.trim()));
    if (values.some((value) => !Number.isFinite(value))) {
      throw new TypeError(label + ' only accepts finite numbers');
    }
    return values;
  }

  function matrixFromCssTransform(input) {
    const value = String(input).trim();
    if (value === '' || value.toLowerCase() === 'none') return identityMatrix();
    let match = value.match(/^matrix\((.*)\)$/i);
    if (match) {
      const values = parseMatrixNumberList(match[1], 6, 'matrix()');
      return matrixFromInit(values);
    }
    match = value.match(/^matrix3d\((.*)\)$/i);
    if (match) {
      return parseMatrixNumberList(match[1], 16, 'matrix3d()');
    }
    throw new TypeError('DOMMatrix.setMatrixValue only supports none, matrix(), and matrix3d()');
  }

  function translatedMatrix(tx, ty, tz) {
    const m = identityMatrix();
    m[12] = tx; m[13] = ty; m[14] = tz;
    return m;
  }

  function scaledMatrix(sx, sy, sz) {
    const m = identityMatrix();
    m[0] = sx; m[5] = sy; m[10] = sz;
    return m;
  }

  function rotationMatrix(angle) {
    const t = finiteNumber(angle, 0) * Math.PI / 180;
    const c = Math.cos(t), s = Math.sin(t);
    return [c, s, 0, 0, -s, c, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  }

  function skewXMatrix(angle) {
    const t = Math.tan(finiteNumber(angle, 0) * Math.PI / 180);
    return [1, 0, 0, 0, t, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  }

  function skewYMatrix(angle) {
    const t = Math.tan(finiteNumber(angle, 0) * Math.PI / 180);
    return [1, t, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  }

  class VixenDOMMatrixReadOnly {
    constructor(init) {
      defineReadonly(this, '__vixenMatrix', matrixFromInit(init), false);
    }
    get m11() { return this.__vixenMatrix[0]; } get a() { return this.m11; }
    get m12() { return this.__vixenMatrix[1]; } get b() { return this.m12; }
    get m13() { return this.__vixenMatrix[2]; }
    get m14() { return this.__vixenMatrix[3]; }
    get m21() { return this.__vixenMatrix[4]; } get c() { return this.m21; }
    get m22() { return this.__vixenMatrix[5]; } get d() { return this.m22; }
    get m23() { return this.__vixenMatrix[6]; }
    get m24() { return this.__vixenMatrix[7]; }
    get m31() { return this.__vixenMatrix[8]; }
    get m32() { return this.__vixenMatrix[9]; }
    get m33() { return this.__vixenMatrix[10]; }
    get m34() { return this.__vixenMatrix[11]; }
    get m41() { return this.__vixenMatrix[12]; } get e() { return this.m41; }
    get m42() { return this.__vixenMatrix[13]; } get f() { return this.m42; }
    get m43() { return this.__vixenMatrix[14]; }
    get m44() { return this.__vixenMatrix[15]; }
    get is2D() {
      const m = this.__vixenMatrix;
      return m[2] === 0 && m[3] === 0 && m[6] === 0 && m[7] === 0 &&
        m[8] === 0 && m[9] === 0 && m[11] === 0 && m[14] === 0 && m[10] === 1 && m[15] === 1;
    }
    get isIdentity() { return this.__vixenMatrix.every((v, i) => v === identityMatrix()[i]); }
    _new(matrix) {
      const out = new VixenDOMMatrix();
      out.__vixenMatrix.splice(0, 16, ...matrix);
      return out;
    }
    multiply(other = undefined) { return this._new(multiplyMatrix(this.__vixenMatrix, matrixFromInit(other))); }
    translate(tx = 0, ty = 0, tz = 0) { return this._new(multiplyMatrix(this.__vixenMatrix, translatedMatrix(finiteNumber(tx), finiteNumber(ty), finiteNumber(tz)))); }
    scale(sx = 1, sy = sx, sz = 1) { return this._new(multiplyMatrix(this.__vixenMatrix, scaledMatrix(finiteNumber(sx, 1), finiteNumber(sy, finiteNumber(sx, 1)), finiteNumber(sz, 1)))); }
    rotate(angle = 0) { return this._new(multiplyMatrix(this.__vixenMatrix, rotationMatrix(angle))); }
    skewX(angle = 0) { return this._new(multiplyMatrix(this.__vixenMatrix, skewXMatrix(angle))); }
    skewY(angle = 0) { return this._new(multiplyMatrix(this.__vixenMatrix, skewYMatrix(angle))); }
    flipX() { return this.scale(-1, 1, 1); }
    flipY() { return this.scale(1, -1, 1); }
    inverse() { return this._new(inverseMatrix(this.__vixenMatrix)); }
    transformPoint(point = {}) {
      const p = pointInit(point);
      const m = this.__vixenMatrix;
      const x = m[0] * p.x + m[4] * p.y + m[8] * p.z + m[12] * p.w;
      const y = m[1] * p.x + m[5] * p.y + m[9] * p.z + m[13] * p.w;
      const z = m[2] * p.x + m[6] * p.y + m[10] * p.z + m[14] * p.w;
      const w = m[3] * p.x + m[7] * p.y + m[11] * p.z + m[15] * p.w;
      return w === 0 || !Number.isFinite(w) ? new VixenDOMPoint(x, y, z, w) : new VixenDOMPoint(x / w, y / w, z / w, 1);
    }
    toFloat32Array() { return new Float32Array(this.__vixenMatrix); }
    toFloat64Array() { return new Float64Array(this.__vixenMatrix); }
    toJSON() {
      const out = {};
      for (const name of ['a','b','c','d','e','f','m11','m12','m13','m14','m21','m22','m23','m24','m31','m32','m33','m34','m41','m42','m43','m44','is2D','isIdentity']) out[name] = this[name];
      return out;
    }
  }

  class VixenDOMMatrix extends VixenDOMMatrixReadOnly {
    static fromMatrix(init = {}) { return new VixenDOMMatrix(init); }
    static fromFloat32Array(array) { return new VixenDOMMatrix(array); }
    static fromFloat64Array(array) { return new VixenDOMMatrix(array); }
    _replace(matrix) { this.__vixenMatrix.splice(0, 16, ...matrix); return this; }
    multiplySelf(other) { return this._replace(multiplyMatrix(this.__vixenMatrix, matrixFromInit(other))); }
    preMultiplySelf(other) { return this._replace(multiplyMatrix(matrixFromInit(other), this.__vixenMatrix)); }
    translateSelf(tx = 0, ty = 0, tz = 0) { return this._replace(this.translate(tx, ty, tz).__vixenMatrix); }
    scaleSelf(sx = 1, sy = sx, sz = 1) { return this._replace(this.scale(sx, sy, sz).__vixenMatrix); }
    rotateSelf(angle = 0) { return this._replace(this.rotate(angle).__vixenMatrix); }
    skewXSelf(angle = 0) { return this._replace(this.skewX(angle).__vixenMatrix); }
    skewYSelf(angle = 0) { return this._replace(this.skewY(angle).__vixenMatrix); }
    invertSelf() { return this._replace(this.inverse().__vixenMatrix); }
    setMatrixValue(transformList = '') { return this._replace(matrixFromCssTransform(transformList)); }
  }

  copyPrototypeMembers(VixenDOMMatrixReadOnly.prototype, VixenDOMMatrix.prototype);

  webidl.adoptInterface('DOMPointReadOnly', VixenDOMPointReadOnly);
  webidl.adoptInterface('DOMPoint', VixenDOMPoint);
  webidl.adoptInterface('DOMRectReadOnly', VixenDOMRectReadOnly);
  webidl.adoptInterface('DOMRect', VixenDOMRect);
  webidl.adoptInterface('DOMQuad', VixenDOMQuad);
  webidl.adoptInterface('DOMMatrixReadOnly', VixenDOMMatrixReadOnly);
  webidl.adoptInterface('DOMMatrix', VixenDOMMatrix);

  // -----------------------------------------------------------------------
  // URL / URLSearchParams / URLPattern
  // -----------------------------------------------------------------------

  function decodeParam(value) {
    return decodeURIComponent(String(value).replace(/\+/g, ' '));
  }

  function encodeParam(value) {
    return encodeURIComponent(String(value)).replace(/%20/g, '+');
  }

  class VixenURLSearchParams {
    constructor(init = '') {
      defineReadonly(this, '__vixenPairs', [], false);
      if (typeof init === 'string') {
        let input = init.startsWith('?') ? init.slice(1) : init;
        if (input !== '') {
          for (const part of input.split('&')) {
            const [name, value = ''] = part.split('=');
            this.append(decodeParam(name), decodeParam(value));
          }
        }
      } else if (init && typeof init[Symbol.iterator] === 'function') {
        for (const pair of init) this.append(pair[0], pair[1]);
      } else if (init && typeof init === 'object') {
        for (const [name, value] of Object.entries(init)) this.append(name, value);
      }
    }
    get size() { return this.__vixenPairs.length; }
    append(name, value) { this.__vixenPairs.push([String(name), String(value)]); }
    delete(name) { const n = String(name); for (let i = this.__vixenPairs.length - 1; i >= 0; i--) if (this.__vixenPairs[i][0] === n) this.__vixenPairs.splice(i, 1); }
    get(name) { const n = String(name); const pair = this.__vixenPairs.find(([key]) => key === n); return pair ? pair[1] : null; }
    getAll(name) { const n = String(name); return this.__vixenPairs.filter(([key]) => key === n).map(([, value]) => value); }
    has(name, value = undefined) { const n = String(name); return this.__vixenPairs.some(([key, val]) => key === n && (value === undefined || val === String(value))); }
    set(name, value) {
      const n = String(name), v = String(value);
      let found = false;
      for (let i = this.__vixenPairs.length - 1; i >= 0; i--) {
        if (this.__vixenPairs[i][0] === n) {
          if (!found) { this.__vixenPairs[i][1] = v; found = true; }
          else this.__vixenPairs.splice(i, 1);
        }
      }
      if (!found) this.append(n, v);
    }
    sort() { this.__vixenPairs.sort((a, b) => a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0); }
    entries() { return this.__vixenPairs.map((pair) => pair.slice())[Symbol.iterator](); }
    keys() { return this.__vixenPairs.map(([name]) => name)[Symbol.iterator](); }
    values() { return this.__vixenPairs.map(([, value]) => value)[Symbol.iterator](); }
    forEach(callback, thisArg = undefined) { for (const [name, value] of this.__vixenPairs) callback.call(thisArg, value, name, this); }
    toString() { return this.__vixenPairs.map(([name, value]) => encodeParam(name) + '=' + encodeParam(value)).join('&'); }
    [Symbol.iterator]() { return this.entries(); }
  }

  function parseUrl(input, base = undefined) {
    input = String(input);
    if (base !== undefined && !/^[A-Za-z][A-Za-z0-9+.-]*:/.test(input)) {
      const b = parseUrl(base);
      if (input.startsWith('/')) input = b.protocol + '//' + b.host + input;
      else {
        const dir = b.pathname.replace(/[^/]*$/, '');
        input = b.protocol + '//' + b.host + dir + input;
      }
    }
    const data = input.match(/^([A-Za-z][A-Za-z0-9+.-]*):(.*)$/);
    if (!data) throw new TypeError('Invalid URL');
    const scheme = data[1].toLowerCase();
    if (scheme === 'data') {
      return { protocol: 'data:', username: '', password: '', host: '', hostname: '', port: '', pathname: data[2], search: '', hash: '', origin: 'null', href: 'data:' + data[2] };
    }
    const match = input.match(/^([A-Za-z][A-Za-z0-9+.-]*):\/\/([^/?#]*)([^?#]*)(\?[^#]*)?(#.*)?$/);
    if (!match) throw new TypeError('Invalid URL');
    let authority = match[2];
    let username = '', password = '';
    if (authority.includes('@')) {
      const parts = authority.split('@');
      const auth = parts.shift();
      authority = parts.join('@');
      const [u, p = ''] = auth.split(':');
      username = u; password = p;
    }
    let hostname = authority, port = '';
    if (authority.startsWith('[')) {
      const end = authority.indexOf(']');
      hostname = authority.slice(0, end + 1);
      if (authority.slice(end + 1).startsWith(':')) port = authority.slice(end + 2);
    } else if (authority.includes(':')) {
      const pieces = authority.split(':');
      port = pieces.pop();
      hostname = pieces.join(':');
    }
    const pathname = match[3] || '/';
    const search = match[4] || '';
    const hash = match[5] || '';
    const host = hostname + (port ? ':' + port : '');
    const protocol = scheme + ':';
    return { protocol, username, password, host, hostname, port, pathname, search, hash, origin: protocol + '//' + host, href: protocol + '//' + host + pathname + search + hash };
  }

  class VixenURL {
    constructor(input, base = undefined) {
      const parsed = parseUrl(input, base);
      for (const key of Object.keys(parsed)) defineReadonly(this, key === 'href' ? '__vixenHref' : '__vixen' + key[0].toUpperCase() + key.slice(1), parsed[key], false);
      defineReadonly(this, 'searchParams', new VixenURLSearchParams(parsed.search), true);
    }
    get href() { return this.__vixenHref; }
    get origin() { return this.__vixenOrigin; }
    get protocol() { return this.__vixenProtocol; }
    get username() { return this.__vixenUsername; }
    get password() { return this.__vixenPassword; }
    get host() { return this.__vixenHost; }
    get hostname() { return this.__vixenHostname; }
    get port() { return this.__vixenPort; }
    get pathname() { return this.__vixenPathname; }
    get search() { return this.__vixenSearch; }
    get hash() { return this.__vixenHash; }
    toString() { return this.href; }
    toJSON() { return this.href; }
    static canParse(input, base = undefined) { try { parseUrl(input, base); return true; } catch (_) { return false; } }
    static parse(input, base = undefined) { return new VixenURL(input, base); }
    static createObjectURL() { return 'blob:vixen'; }
    static revokeObjectURL() {}
  }

  class VixenURLPattern {
    constructor(init = {}) { defineReadonly(this, 'pathname', String(init.pathname || '*'), true); }
    _match(input) {
      const path = String((input && input.pathname) || '');
      const pattern = this.pathname;
      if (pattern.endsWith('/*')) {
        const prefix = pattern.slice(0, -1);
        if (!path.startsWith(prefix)) return null;
        return { '*': path.slice(prefix.length) };
      }
      const p = pattern.split('/').filter(Boolean);
      const s = path.split('/').filter(Boolean);
      if (p.length !== s.length) return null;
      const groups = {};
      for (let i = 0; i < p.length; i++) {
        if (p[i].startsWith(':')) groups[p[i].slice(1)] = s[i];
        else if (p[i] !== s[i]) return null;
      }
      return groups;
    }
    test(input) { return this._match(input) !== null; }
    exec(input) { const groups = this._match(input); return groups === null ? null : { pathname: { groups } }; }
  }

  webidl.adoptInterface('URL', VixenURL);
  webidl.adoptInterface('URLSearchParams', VixenURLSearchParams);
  webidl.adoptInterface('URLPattern', VixenURLPattern);

  // -----------------------------------------------------------------------
  // Fetch body value APIs: Headers / Blob / File / Request / Response
  // -----------------------------------------------------------------------

  function normalizeHeaderName(name) {
    const value = String(name);
    if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(value)) throw new TypeError('invalid header name');
    return value.toLowerCase();
  }

  function normalizeHeaderValue(value) {
    const text = String(value);
    if (/[\0\r\n]/.test(text)) throw new TypeError('invalid header value');
    return text.replace(/^[\t ]+|[\t ]+$/g, '');
  }

  function forbiddenRequestHeader(name) {
    name = String(name).toLowerCase();
    return ['accept-charset','accept-encoding','access-control-request-headers','access-control-request-method','connection','content-length','cookie','cookie2','date','dnt','expect','host','keep-alive','origin','referer','set-cookie','te','trailer','transfer-encoding','upgrade','via'].includes(name) || name.startsWith('proxy-') || name.startsWith('sec-');
  }

  function forbiddenResponseHeader(name) {
    name = String(name).toLowerCase();
    return name === 'set-cookie' || name === 'set-cookie2';
  }

  class VixenHeaders {
    constructor(init = undefined) {
      defineReadonly(this, '__vixenEntries', [], false);
      if (init instanceof VixenHeaders) {
        for (const [name, value] of init.__vixenEntries) this.append(name, value);
      } else if (init && typeof init[Symbol.iterator] === 'function') {
        for (const pair of init) this.append(pair[0], pair[1]);
      } else if (init && typeof init === 'object') {
        for (const [name, value] of Object.entries(init)) this.append(name, value);
      }
    }
    append(name, value) { this.__vixenEntries.push([normalizeHeaderName(name), normalizeHeaderValue(value)]); }
    delete(name) { const n = normalizeHeaderName(name); for (let i = this.__vixenEntries.length - 1; i >= 0; i--) if (this.__vixenEntries[i][0] === n) this.__vixenEntries.splice(i, 1); }
    get(name) { const n = normalizeHeaderName(name); const values = this.__vixenEntries.filter(([key]) => key === n).map(([, value]) => value); return values.length ? values.join(', ') : null; }
    getAll(name) { const n = normalizeHeaderName(name); return this.__vixenEntries.filter(([key]) => key === n).map(([, value]) => value); }
    getSetCookie() { return []; }
    has(name) { const n = normalizeHeaderName(name); return this.__vixenEntries.some(([key]) => key === n); }
    set(name, value) { const n = normalizeHeaderName(name); this.delete(n); this.__vixenEntries.push([n, normalizeHeaderValue(value)]); }
    get size() { return Array.from(new Set(this.__vixenEntries.map(([name]) => name))).length; }
    _combined() {
      const out = [];
      for (const [name] of this.__vixenEntries) if (!out.some(([existing]) => existing === name)) out.push([name, this.get(name)]);
      return out;
    }
    entries() { return this._combined()[Symbol.iterator](); }
    keys() { return this._combined().map(([name]) => name)[Symbol.iterator](); }
    values() { return this._combined().map(([, value]) => value)[Symbol.iterator](); }
    forEach(callback, thisArg = undefined) { for (const [name, value] of this._combined()) callback.call(thisArg, value, name, this); }
    [Symbol.iterator]() { return this.entries(); }
  }

  function filteredHeaders(init, forbidden) {
    const headers = new VixenHeaders(init);
    if (!forbidden) return headers;
    const filtered = new VixenHeaders();
    for (const [name, value] of headers.__vixenEntries) if (!forbidden(name)) filtered.append(name, value);
    return filtered;
  }

  function normalizeMime(type) {
    const value = String(type || '');
    return /^[\x20-\x7e]*$/.test(value) ? value.toLowerCase() : '';
  }

  function splitBodyChunks(bytes, sizes = []) {
    const chunks = [];
    let offset = 0;
    for (const rawSize of Array.from(sizes || [])) {
      const size = Math.max(0, Math.min(bytes.length - offset, Math.trunc(finiteNumber(rawSize, 0))));
      if (size === 0) continue;
      chunks.push(bytes.slice(offset, offset + size));
      offset += size;
      if (offset >= bytes.length) break;
    }
    if (offset < bytes.length) chunks.push(bytes.slice(offset));
    return chunks;
  }

  class VixenReadableStream {
    constructor(chunks = [], onLock = null) {
      defineReadonly(this, '__vixenChunks', Array.from(chunks, (chunk) => new Uint8Array(chunk).slice()), false);
      defineData(this, '__vixenIndex', 0, false);
      defineData(this, '__vixenLocked', false, false);
      defineReadonly(this, '__vixenOnLock', typeof onLock === 'function' ? onLock : null, false);
    }
    get locked() { return this.__vixenLocked; }
    cancel() {
      this.__vixenIndex = this.__vixenChunks.length;
      return Promise.resolve();
    }
    getReader(options = undefined) {
      if (options && options.mode !== undefined) throw new TypeError('BYOB readers are not supported for this stream');
      if (this.__vixenLocked) throw new TypeError('ReadableStream is locked');
      this.__vixenLocked = true;
      if (this.__vixenOnLock) this.__vixenOnLock();
      return new VixenReadableStreamDefaultReader(this);
    }
    pipeThrough(transform) {
      this.pipeTo(transform.writable);
      return transform.readable;
    }
    pipeTo(destination) {
      const reader = this.getReader();
      const writer = destination && typeof destination.getWriter === 'function' ? destination.getWriter() : destination;
      const pump = () => reader.read().then((item) => {
        if (item.done) return writer && typeof writer.close === 'function' ? writer.close() : undefined;
        const write = writer && typeof writer.write === 'function' ? writer.write(item.value) : undefined;
        return Promise.resolve(write).then(pump);
      });
      return pump();
    }
    tee() {
      if (this.__vixenLocked) throw new TypeError('ReadableStream is locked');
      const remaining = this.__vixenChunks.slice(this.__vixenIndex);
      this.__vixenLocked = true;
      if (this.__vixenOnLock) this.__vixenOnLock();
      return [new VixenReadableStream(remaining), new VixenReadableStream(remaining)];
    }
  }

  class VixenReadableStreamDefaultReader {
    constructor(stream) {
      defineData(this, '__vixenStream', stream, false);
      defineReadonly(this, 'closed', Promise.resolve(), true);
    }
    read() {
      const stream = this.__vixenStream;
      if (!stream) return Promise.reject(new TypeError('ReadableStream reader is released'));
      if (stream.__vixenIndex >= stream.__vixenChunks.length) {
        return Promise.resolve({ value: undefined, done: true });
      }
      const value = stream.__vixenChunks[stream.__vixenIndex++].slice();
      return Promise.resolve({ value, done: false });
    }
    cancel(reason = undefined) {
      const stream = this.__vixenStream;
      return stream ? stream.cancel(reason) : Promise.resolve();
    }
    releaseLock() {
      if (!this.__vixenStream) return;
      this.__vixenStream.__vixenLocked = false;
      this.__vixenStream = null;
    }
  }

  function installBody(target, info, chunkSizes = []) {
    const bytes = bytesFromString(info.text);
    defineReadonly(target, '__vixenBodyText', info.text, false);
    defineReadonly(target, '__vixenBodyBytes', bytes, false);
    defineReadonly(target, '__vixenChunkSizes', Object.freeze(Array.from(chunkSizes || [], (size) => Math.max(0, Math.trunc(finiteNumber(size, 0))))), false);
    defineData(target, 'bodyUsed', false, true);
    defineReadonly(target, 'body', info.isNull ? null : new VixenReadableStream(splitBodyChunks(bytes, chunkSizes), () => { target.bodyUsed = true; }), true);
  }

  function consumeBody(target, convert) {
    if (target.bodyUsed) return Promise.reject(new TypeError('Body has already been consumed'));
    if (target.body === null) {
      target.bodyUsed = true;
      return Promise.resolve(convert(new Uint8Array(0)));
    }
    const reader = target.body.getReader();
    const chunks = [];
    const read = () => reader.read().then((item) => {
      if (item.done) return convert(concatBytes(chunks));
      chunks.push(item.value);
      return read();
    });
    return read();
  }

  class VixenBlob {
    constructor(parts = [], options = {}) {
      const byteParts = Array.from(parts, bytesFromPart);
      const bytes = concatBytes(byteParts);
      defineReadonly(this, '__vixenParts', byteParts.map((part) => part.slice()), false);
      defineReadonly(this, '__vixenBytes', bytes, false);
      defineReadonly(this, '__vixenText', textFromBytes(bytes), false);
      defineReadonly(this, 'size', bytes.length, true);
      defineReadonly(this, 'type', normalizeMime(options && options.type), true);
    }
    slice(start = 0, end = this.size, type = '') {
      const size = this.size;
      const from = Number.isFinite(Number(start)) ? Number(start) : 0;
      const to = Number.isFinite(Number(end)) ? Number(end) : size;
      const relativeStart = from < 0 ? Math.max(size + Math.trunc(from), 0) : Math.min(Math.trunc(from), size);
      const relativeEnd = to < 0 ? Math.max(size + Math.trunc(to), 0) : Math.min(Math.trunc(to), size);
      return new VixenBlob([this.__vixenBytes.slice(relativeStart, Math.max(relativeStart, relativeEnd))], { type });
    }
    text() { return Promise.resolve(this.__vixenText); }
    arrayBuffer() { return Promise.resolve(this.__vixenBytes.slice().buffer); }
    bytes() { return Promise.resolve(this.__vixenBytes.slice()); }
    stream() { return new VixenReadableStream([this.__vixenBytes]); }
  }

  class VixenFile extends VixenBlob {
    constructor(parts, name, options = {}) {
      super(parts, options);
      defineReadonly(this, 'name', String(name), true);
      defineReadonly(this, 'lastModified', options && options.lastModified !== undefined ? finiteNumber(options.lastModified, 0) : Date.now(), true);
      defineReadonly(this, 'webkitRelativePath', '', true);
    }
  }

  class VixenFileList {
    constructor(files = []) {
      const list = Array.from(files).filter((file) => file instanceof VixenFile);
      defineReadonly(this, '__vixenFiles', Object.freeze(list.slice()), false);
      syncIndexedValues(this, this.__vixenFiles);
    }
    get length() { return this.__vixenFiles.length; }
    item(index) {
      const n = Number(index);
      return Number.isInteger(n) && n >= 0 && n < this.__vixenFiles.length ? this.__vixenFiles[n] : null;
    }
    entries() { return this.__vixenFiles.entries(); }
    keys() { return this.__vixenFiles.keys(); }
    values() { return this.__vixenFiles.values(); }
    [Symbol.iterator]() { return this.values(); }
  }

  class VixenDataTransferItem {
    constructor(kind, type, value) {
      defineReadonly(this, '__vixenValue', value, false);
      defineReadonly(this, 'kind', String(kind), true);
      defineReadonly(this, 'type', normalizeMime(type), true);
    }
    getAsFile() { return this.kind === 'file' ? this.__vixenValue : null; }
    getAsString(callback) {
      if (this.kind !== 'string' || typeof callback !== 'function') return;
      callback(String(this.__vixenValue));
    }
  }

  class VixenDataTransferItemList {
    constructor() {
      defineReadonly(this, '__vixenItems', [], false);
      syncIndexedValues(this, this.__vixenItems);
    }
    get length() { return this.__vixenItems.length; }
    item(index) {
      const n = Number(index);
      return Number.isInteger(n) && n >= 0 && n < this.__vixenItems.length ? this.__vixenItems[n] : null;
    }
    add(data, type = '') {
      let item;
      if (data instanceof VixenFile) item = new VixenDataTransferItem('file', data.type, data);
      else item = new VixenDataTransferItem('string', type, String(data));
      this.__vixenItems.push(item);
      syncIndexedValues(this, this.__vixenItems);
      return item;
    }
    remove(index) {
      const n = Number(index);
      if (Number.isInteger(n) && n >= 0 && n < this.__vixenItems.length) this.__vixenItems.splice(n, 1);
      syncIndexedValues(this, this.__vixenItems);
    }
    clear() {
      this.__vixenItems.splice(0, this.__vixenItems.length);
      syncIndexedValues(this, this.__vixenItems);
    }
    entries() { return this.__vixenItems.entries(); }
    keys() { return this.__vixenItems.keys(); }
    values() { return this.__vixenItems.values(); }
    [Symbol.iterator]() { return this.values(); }
  }

  class VixenDataTransfer {
    constructor() {
      defineReadonly(this, 'items', new VixenDataTransferItemList(), true);
      defineData(this, 'dropEffect', 'none', true);
      defineData(this, 'effectAllowed', 'all', true);
    }
    get files() {
      return new VixenFileList(this.items.__vixenItems.map((item) => item.getAsFile()).filter((file) => file !== null));
    }
    get types() {
      const types = [];
      for (const item of this.items) {
        const type = item.kind === 'file' ? 'Files' : item.type;
        if (type && !types.includes(type)) types.push(type);
      }
      return Object.freeze(types);
    }
    getData(type) {
      const normalized = normalizeMime(type);
      const item = this.items.__vixenItems.find((entry) => entry.kind === 'string' && entry.type === normalized);
      return item ? String(item.__vixenValue) : '';
    }
    setData(type, data) {
      const normalized = normalizeMime(type);
      this.clearData(normalized);
      this.items.__vixenItems.push(new VixenDataTransferItem('string', normalized, String(data)));
      syncIndexedValues(this.items, this.items.__vixenItems);
    }
    clearData(type = undefined) {
      if (type === undefined) this.items.clear();
      else {
        const normalized = normalizeMime(type);
        for (let i = this.items.__vixenItems.length - 1; i >= 0; i--) {
          if (this.items.__vixenItems[i].kind === 'string' && this.items.__vixenItems[i].type === normalized) {
            this.items.__vixenItems.splice(i, 1);
          }
        }
        syncIndexedValues(this.items, this.items.__vixenItems);
      }
    }
    setDragImage() {}
  }

  function bodyInfo(body) {
    if (body === null || body === undefined) return { isNull: true, contentType: '', text: '' };
    if (body instanceof VixenBlob) return { isNull: false, contentType: body.type, text: body.__vixenText };
    if (body instanceof VixenURLSearchParams) return { isNull: false, contentType: 'application/x-www-form-urlencoded;charset=UTF-8', text: body.toString() };
    if (body instanceof ArrayBuffer || (ArrayBuffer.isView && ArrayBuffer.isView(body))) {
      return { isNull: false, contentType: '', text: textFromBytes(bytesFromPart(body)) };
    }
    return { isNull: false, contentType: 'text/plain;charset=UTF-8', text: String(body) };
  }

  class VixenRequest {
    constructor(input, init = {}) {
      const source = input instanceof VixenRequest ? input : null;
      const url = source ? source.url : new VixenURL(String(input)).href;
      const method = String((init && init.method) || (source && source.method) || 'GET').toUpperCase();
      const body = bodyInfo(init && Object.prototype.hasOwnProperty.call(init, 'body') ? init.body : (source && source.body !== null ? source.__vixenBodyBytes : null));
      if (!body.isNull && (method === 'GET' || method === 'HEAD')) throw new TypeError('Request GET/HEAD cannot have a body');
      const headers = filteredHeaders((init && init.headers) || (source && source.headers) || undefined, forbiddenRequestHeader);
      if (!body.isNull && body.contentType && !headers.has('content-type')) headers.set('Content-Type', body.contentType);
      defineReadonly(this, 'url', url, true);
      defineReadonly(this, 'method', method, true);
      defineReadonly(this, 'headers', headers, true);
      defineReadonly(this, 'destination', '', true);
      defineReadonly(this, 'referrer', (init && init.referrer) || 'about:client', true);
      defineReadonly(this, 'referrerPolicy', (init && init.referrerPolicy) || '', true);
      defineReadonly(this, 'mode', (init && init.mode) || 'cors', true);
      defineReadonly(this, 'credentials', init && Object.prototype.hasOwnProperty.call(init, 'credentials') ? init.credentials : (source && source.credentials) || 'same-origin', true);
      defineReadonly(this, 'cache', init && Object.prototype.hasOwnProperty.call(init, 'cache') ? init.cache : (source && source.cache) || 'default', true);
      defineReadonly(this, 'redirect', init && Object.prototype.hasOwnProperty.call(init, 'redirect') ? init.redirect : (source && source.redirect) || 'follow', true);
      defineReadonly(this, 'integrity', (init && init.integrity) || (source && source.integrity) || '', true);
      defineReadonly(this, 'keepalive', Boolean(init && init.keepalive), true);
      defineReadonly(this, 'signal', (init && init.signal) || (source && source.signal) || new VixenAbortController().signal, true);
      installBody(this, body);
    }
    clone() {
      if (this.bodyUsed) throw new TypeError('Cannot clone a consumed Request body');
      return new VixenRequest(this);
    }
    text() { return consumeBody(this, textFromBytes); }
    json() { return this.text().then((text) => text === '' ? null : JSON.parse(text)); }
    blob() { return consumeBody(this, (bytes) => new VixenBlob([bytes])); }
    arrayBuffer() { return consumeBody(this, (bytes) => bytes.slice().buffer); }
    bytes() { return consumeBody(this, (bytes) => bytes.slice()); }
    formData() { return Promise.reject(new TypeError('multipart body decoding is not supported')); }
  }

  class VixenResponse {
    constructor(body = null, init = {}) {
      const info = bodyInfo(body);
      const status = init && init.status !== undefined ? finiteNumber(init.status, 200) : 200;
      const headers = filteredHeaders(init && init.headers, forbiddenResponseHeader);
      if (!info.isNull && info.contentType && !headers.has('content-type')) headers.set('Content-Type', info.contentType);
      defineReadonly(this, 'type', (init && init.type) || 'default', true);
      defineReadonly(this, 'url', (init && init.url) || '', true);
      defineReadonly(this, 'redirected', Boolean(init && init.redirected), true);
      defineReadonly(this, 'status', status, true);
      defineReadonly(this, 'ok', status >= 200 && status <= 299, true);
      defineReadonly(this, 'statusText', (init && init.statusText) || '', true);
      defineReadonly(this, 'headers', headers, true);
      installBody(this, info, init && init.bodyChunks);
    }
    clone() {
      if (this.bodyUsed) throw new TypeError('Cannot clone a consumed Response body');
      return new VixenResponse(this.__vixenBodyBytes, { status: this.status, statusText: this.statusText, type: this.type, headers: this.headers, url: this.url, redirected: this.redirected, bodyChunks: this.__vixenChunkSizes });
    }
    text() { return consumeBody(this, textFromBytes); }
    json() { return this.text().then((text) => text === '' ? null : JSON.parse(text)); }
    blob() { return consumeBody(this, (bytes) => new VixenBlob([bytes], { type: this.headers.get('content-type') || '' })); }
    arrayBuffer() { return consumeBody(this, (bytes) => bytes.slice().buffer); }
    bytes() { return consumeBody(this, (bytes) => bytes.slice()); }
    formData() { return Promise.reject(new TypeError('multipart body decoding is not supported')); }
    static error() {
      const response = new VixenResponse(null, { status: 200 });
      Object.defineProperty(response, 'type', { value: 'error', configurable: true });
      Object.defineProperty(response, 'status', { value: 0, configurable: true });
      Object.defineProperty(response, 'ok', { value: false, configurable: true });
      return response;
    }
    static json(data, init = {}) {
      const nextInit = Object.assign({}, init || {});
      const headers = filteredHeaders(nextInit.headers, forbiddenResponseHeader);
      if (!headers.has('content-type')) headers.set('Content-Type', 'application/json');
      nextInit.headers = headers;
      return new VixenResponse(JSON.stringify(data), nextInit);
    }
    static redirect(url, status = 302) { return new VixenResponse(null, { status, headers: [['Location', new VixenURL(url).href]] }); }
  }

  webidl.adoptInterface('Headers', VixenHeaders);
  webidl.adoptInterface('Blob', VixenBlob);
  webidl.adoptInterface('File', VixenFile);
  webidl.adoptInterface('FileList', VixenFileList);
  webidl.adoptInterface('DataTransferItem', VixenDataTransferItem);
  webidl.adoptInterface('DataTransferItemList', VixenDataTransferItemList);
  webidl.adoptInterface('DataTransfer', VixenDataTransfer);
  webidl.adoptInterface('Request', VixenRequest);
  webidl.adoptInterface('Response', VixenResponse);
  webidl.adoptInterface('ReadableStream', VixenReadableStream);
  webidl.adoptInterface('ReadableStreamDefaultReader', VixenReadableStreamDefaultReader);

  function fetch(input, init = {}) {
    let request;
    try {
      request = new VixenRequest(input, init);
    } catch (err) {
      return Promise.reject(err);
    }
    if (request.signal && request.signal.aborted) {
      return Promise.reject(request.signal.reason === undefined ? new TypeError('fetch aborted') : request.signal.reason);
    }
    const started = op_vixen_fetch_start({ url: request.url, method: request.method, mode: request.mode, cache: request.cache, credentials: request.credentials, redirect: request.redirect, referrerPolicy: request.referrerPolicy, integrity: request.integrity, headers: Array.from(request.headers.entries()), body: request.body === null ? null : request.__vixenBodyText });
    if (!started || !started.ok) {
      return Promise.reject(new TypeError(started && started.message ? started.message : 'fetch failed to start'));
    }
    const abort = () => { op_vixen_fetch_cancel(started.id); };
    if (request.signal) request.signal.addEventListener('abort', abort, { once: true });
    const pending = op_vixen_fetch_finish(started.id).then((result) => {
      if (request.signal) request.signal.removeEventListener('abort', abort);
      recordNetworkEvents(result && result.events);
      if (request.signal && request.signal.aborted) {
        throw request.signal.reason === undefined ? new TypeError('fetch aborted') : request.signal.reason;
      }
      if (!result || !result.ok) {
        throw new TypeError(result && result.message ? result.message : 'fetch failed');
      }
      return new VixenResponse(result.responseType === 'opaque' ? null : result.body, {
        status: result.status,
        type: result.responseType,
        headers: result.headers,
        url: result.finalUrl,
        redirected: result.redirected,
        bodyChunks: result.bodyChunks,
      });
    }, (error) => {
      if (request.signal) request.signal.removeEventListener('abort', abort);
      if (request.signal && request.signal.aborted) {
        throw request.signal.reason === undefined ? new TypeError('fetch aborted') : request.signal.reason;
      }
      throw error;
    });
    pending.catch(() => {});
    return pending;
  }

  defineGlobal('fetch', fetch);

  class VixenXMLHttpRequestUpload extends VixenEventTarget {}

  function fireXhrEvent(target, type) {
    const event = new Event(type);
    target.dispatchEvent(event);
  }

  function fireXhrProgress(target, type, loaded, total) {
    target.dispatchEvent(new VixenProgressEvent(type, {
      lengthComputable: Number.isFinite(total),
      loaded,
      total: Number.isFinite(total) ? total : 0,
    }));
  }

  function setXhrReadyState(xhr, readyState) {
    xhr.readyState = readyState;
    fireXhrEvent(xhr, 'readystatechange');
  }

  class VixenXMLHttpRequest extends VixenEventTarget {
    constructor() {
      super();
      defineData(this, 'readyState', 0, true);
      defineData(this, 'timeout', 0, true);
      defineData(this, 'withCredentials', false, true);
      defineReadonly(this, 'upload', new VixenXMLHttpRequestUpload(), true);
      defineData(this, 'responseURL', '', true);
      defineData(this, 'status', 0, true);
      defineData(this, 'statusText', '', true);
      defineData(this, 'responseType', '', true);
      defineData(this, 'response', '', true);
      defineData(this, 'responseText', '', true);
      defineData(this, 'responseXML', null, true);
      defineData(this, '__vixenHeaders', new VixenHeaders(), false);
      defineData(this, '__vixenMethod', 'GET', false);
      defineData(this, '__vixenUrl', '', false);
      defineData(this, '__vixenAsync', true, false);
      defineData(this, '__vixenResponseHeaders', new VixenHeaders(), false);
      defineData(this, '__vixenAborted', false, false);
      defineData(this, '__vixenController', null, false);
      defineData(this, '__vixenSendGeneration', 0, false);
    }
    open(method, url, async = true) {
      if (this.__vixenController) this.__vixenController.abort();
      this.__vixenController = null;
      this.__vixenSendGeneration++;
      this.__vixenMethod = String(method || 'GET').toUpperCase();
      this.__vixenUrl = new VixenURL(String(url), typeof location !== 'undefined' ? location.href : undefined).href;
      this.__vixenAsync = async !== false;
      this.__vixenHeaders = new VixenHeaders();
      this.__vixenResponseHeaders = new VixenHeaders();
      this.__vixenAborted = false;
      this.status = 0;
      this.statusText = '';
      this.responseURL = '';
      this.response = '';
      this.responseText = '';
      setXhrReadyState(this, 1);
    }
    setRequestHeader(name, value) {
      if (this.readyState !== 1) throw new Error('XMLHttpRequest is not open');
      this.__vixenHeaders.append(name, value);
    }
    send(body = null) {
      if (this.readyState !== 1) throw new Error('XMLHttpRequest is not open');
      if (!this.__vixenAsync) throw new Error('synchronous XMLHttpRequest is not supported');
      this.__vixenAborted = false;
      const generation = ++this.__vixenSendGeneration;
      const controller = new VixenAbortController();
      this.__vixenController = controller;
      fireXhrProgress(this, 'loadstart', 0, 0);
      const init = {
        method: this.__vixenMethod,
        headers: this.__vixenHeaders,
        credentials: this.withCredentials ? 'include' : 'same-origin',
        signal: controller.signal,
      };
      if (body !== null && body !== undefined) {
        init.body = body;
        const uploadBytes = bytesFromPart(body).length;
        fireXhrProgress(this.upload, 'loadstart', 0, uploadBytes);
        fireXhrProgress(this.upload, 'progress', uploadBytes, uploadBytes);
        fireXhrProgress(this.upload, 'load', uploadBytes, uploadBytes);
        fireXhrProgress(this.upload, 'loadend', uploadBytes, uploadBytes);
      }
      fetch(this.__vixenUrl, init).then((res) => {
        if (this.__vixenAborted || generation !== this.__vixenSendGeneration) return;
        this.status = res.status;
        this.statusText = res.statusText || '';
        this.responseURL = res.url || '';
        this.__vixenResponseHeaders = new VixenHeaders(res.headers);
        setXhrReadyState(this, 2);
        const total = res.__vixenBodyBytes.length;
        let loaded = 0;
        for (const chunkSize of res.__vixenChunkSizes) {
          loaded = Math.min(total, loaded + chunkSize);
          fireXhrProgress(this, 'progress', loaded, total);
        }
        if (loaded < total) fireXhrProgress(this, 'progress', total, total);
        return res.text().then((text) => {
          if (this.__vixenAborted || generation !== this.__vixenSendGeneration) return;
          this.responseText = text;
          this.response = text;
          setXhrReadyState(this, 3);
          setXhrReadyState(this, 4);
          fireXhrProgress(this, 'load', total, total);
          fireXhrProgress(this, 'loadend', total, total);
          if (generation === this.__vixenSendGeneration) this.__vixenController = null;
        });
      }).catch((err) => {
        if (this.__vixenAborted || generation !== this.__vixenSendGeneration) return;
        this.status = 0;
        this.statusText = '';
        this.responseText = '';
        this.response = '';
        setXhrReadyState(this, 4);
        fireXhrEvent(this, 'error');
        fireXhrEvent(this, 'loadend');
        if (generation === this.__vixenSendGeneration) this.__vixenController = null;
      });
    }
    abort() {
      this.__vixenAborted = true;
      this.__vixenSendGeneration++;
      if (this.__vixenController) this.__vixenController.abort();
      this.__vixenController = null;
      this.status = 0;
      this.statusText = '';
      setXhrReadyState(this, 0);
      fireXhrEvent(this, 'abort');
      fireXhrEvent(this, 'loadend');
    }
    getResponseHeader(name) {
      if (this.readyState < 2) return null;
      return this.__vixenResponseHeaders.get(name);
    }
    getAllResponseHeaders() {
      if (this.readyState < 2) return '';
      return Array.from(this.__vixenResponseHeaders.entries()).map(([name, value]) => `${name}: ${value}\r\n`).join('');
    }
    overrideMimeType() {}
  }

  for (const [name, value] of [['UNSENT', 0], ['OPENED', 1], ['HEADERS_RECEIVED', 2], ['LOADING', 3], ['DONE', 4]]) {
    defineReadonly(VixenXMLHttpRequest, name, value, true);
    defineReadonly(VixenXMLHttpRequest.prototype, name, value, true);
  }
  webidl.adoptInterface('XMLHttpRequestUpload', VixenXMLHttpRequestUpload);
  webidl.adoptInterface('XMLHttpRequest', VixenXMLHttpRequest);
  defineGlobal('XMLHttpRequest', VixenXMLHttpRequest);

  // -----------------------------------------------------------------------
  // Abort, MutationObserver, structuredClone, DOMParser, platform globals
  // -----------------------------------------------------------------------

  function abortReason(name, message) {
    const error = new Error(message);
    error.name = name;
    return error;
  }

  class VixenAbortSignal extends VixenEventTarget {
    constructor(aborted = false, reason = undefined) { super(); defineReadonly(this, '__vixenAbortState', { aborted, reason }, false); }
    get aborted() { return this.__vixenAbortState.aborted; }
    get reason() { return this.__vixenAbortState.reason; }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    __vixenAbort(reason = undefined) {
      if (this.aborted) return;
      this.__vixenAbortState.aborted = true;
      this.__vixenAbortState.reason = reason === undefined ? abortReason('AbortError', 'The operation was aborted') : reason;
      this.dispatchEvent(new VixenEvent('abort'));
    }
    static abort(reason = undefined) {
      const signal = new VixenAbortSignal(false);
      signal.__vixenAbort(reason);
      return signal;
    }
    static timeout(ms) {
      const delay = Math.max(0, Math.trunc(finiteNumber(ms, 0)));
      const signal = new VixenAbortSignal(false);
      const expire = () => signal.__vixenAbort(abortReason('TimeoutError', 'The operation timed out'));
      if (delay === 0 || typeof globalThis.setTimeout !== 'function') expire();
      else globalThis.setTimeout(expire, delay);
      return signal;
    }
    static any(signals) {
      const signal = new VixenAbortSignal(false);
      for (const source of Array.from(signals)) {
        if (!source || typeof source.addEventListener !== 'function') throw new TypeError('AbortSignal.any expects signals');
        if (source.aborted) {
          signal.__vixenAbort(source.reason);
          break;
        }
        source.addEventListener('abort', () => signal.__vixenAbort(source.reason), { once: true });
      }
      return signal;
    }
  }

  class VixenAbortController {
    constructor() { defineReadonly(this, 'signal', new VixenAbortSignal(false), true); }
    abort(reason = undefined) { this.signal.__vixenAbort(reason); }
  }

  const mutationObservers = new Set();
  let mutationDeliveryScheduled = false;

  function queueMicrotaskCompat(callback) {
    Promise.resolve().then(callback);
  }

  if (typeof globalThis.queueMicrotask !== 'function') {
    defineGlobal('queueMicrotask', (callback) => {
      if (typeof callback !== 'function') throw new TypeError('queueMicrotask callback must be a function');
      queueMicrotaskCompat(callback);
    });
  }

  function mutationNodeList(nodes) {
    const hook = globalThis.__vixenMakeNodeList;
    if (typeof hook === 'function') return hook(nodes || []);
    const list = Array.from(nodes || []);
    list.item = function (index) {
      const n = Number(index);
      return Number.isInteger(n) && n >= 0 && n < list.length ? list[n] : null;
    };
    return list;
  }

  class VixenMutationRecord {
    constructor(record) {
      defineReadonly(this, 'type', record.type, true);
      defineReadonly(this, 'target', record.target || null, true);
      defineReadonly(this, 'addedNodes', mutationNodeList(record.addedNodes), true);
      defineReadonly(this, 'removedNodes', mutationNodeList(record.removedNodes), true);
      defineReadonly(this, 'previousSibling', record.previousSibling || null, true);
      defineReadonly(this, 'nextSibling', record.nextSibling || null, true);
      defineReadonly(this, 'attributeName', record.attributeName || null, true);
      defineReadonly(this, 'attributeNamespace', record.attributeNamespace || null, true);
      defineReadonly(this, 'oldValue', record.oldValue === undefined ? null : record.oldValue, true);
    }
  }

  function normalizeMutationOptions(options = {}) {
    const normalized = {
      childList: Boolean(options.childList),
      attributes: Boolean(options.attributes),
      attributeFilter: Array.isArray(options.attributeFilter)
        ? options.attributeFilter.map((name) => String(name).toLowerCase())
        : null,
      attributeOldValue: Boolean(options.attributeOldValue),
      characterData: Boolean(options.characterData),
      characterDataOldValue: Boolean(options.characterDataOldValue),
      subtree: Boolean(options.subtree),
    };
    if (normalized.attributeOldValue || normalized.attributeFilter !== null) normalized.attributes = true;
    if (normalized.characterDataOldValue) normalized.characterData = true;
    if (!normalized.childList && !normalized.attributes && !normalized.characterData) {
      throw new TypeError('MutationObserver.observe requires childList, attributes, or characterData');
    }
    return normalized;
  }

  function mutationRecordMatchesOptions(record, options) {
    if (record.type === 'childList') return options.childList;
    if (record.type === 'attributes') {
      if (!options.attributes) return false;
      return options.attributeFilter === null || options.attributeFilter.includes(String(record.attributeName || '').toLowerCase());
    }
    if (record.type === 'characterData') return options.characterData;
    return false;
  }

  function mutationTargetInScope(root, target, options) {
    if (root === target) return true;
    if (!options.subtree) return false;
    const hook = globalThis.__vixenNodeContains;
    return typeof hook === 'function' && hook(root, target);
  }

  function tailorMutationRecord(record, options) {
    const tailored = Object.assign({}, record);
    if (record.type === 'attributes' && !options.attributeOldValue) tailored.oldValue = null;
    if (record.type === 'characterData' && !options.characterDataOldValue) tailored.oldValue = null;
    return new VixenMutationRecord(tailored);
  }

  function scheduleMutationDelivery() {
    if (mutationDeliveryScheduled) return;
    mutationDeliveryScheduled = true;
    queueMicrotaskCompat(() => {
      mutationDeliveryScheduled = false;
      for (const observer of Array.from(mutationObservers)) observer.__vixenDeliver();
    });
  }

  function queueMutationRecord(record) {
    for (const observer of mutationObservers) {
      for (const registration of observer.__vixenRegistrations) {
        if (!mutationTargetInScope(registration.target, record.target, registration.options)) continue;
        if (!mutationRecordMatchesOptions(record, registration.options)) continue;
        observer.__vixenRecords.push(tailorMutationRecord(record, registration.options));
        break;
      }
    }
    if ([...mutationObservers].some((observer) => observer.__vixenRecords.length > 0)) {
      scheduleMutationDelivery();
    }
  }

  class VixenMutationObserver {
    constructor(callback) {
      if (typeof callback !== 'function') throw new TypeError('MutationObserver callback must be a function');
      defineReadonly(this, '__vixenCallback', callback, false);
      defineReadonly(this, '__vixenRecords', [], false);
      defineReadonly(this, '__vixenRegistrations', [], false);
      mutationObservers.add(this);
    }
    observe(target, options = {}) {
      if (target === null || (typeof target !== 'object' && typeof target !== 'function')) {
        throw new TypeError('MutationObserver.observe target must be a Node');
      }
      const normalized = normalizeMutationOptions(options);
      const existing = this.__vixenRegistrations.find((registration) => registration.target === target);
      if (existing) existing.options = normalized;
      else this.__vixenRegistrations.push({ target, options: normalized });
    }
    disconnect() { this.__vixenRegistrations.splice(0, this.__vixenRegistrations.length); }
    takeRecords() { return this.__vixenRecords.splice(0, this.__vixenRecords.length); }
    __vixenDeliver() {
      const records = this.takeRecords();
      if (records.length === 0) return;
      this.__vixenCallback.call(this, records, this);
    }
  }

  function cloneValue(value, seen = new Map()) {
    if (value === null || typeof value !== 'object') return value;
    if (seen.has(value)) return seen.get(value);
    if (value instanceof Date) return new Date(value.getTime());
    if (value instanceof Map) {
      const out = new Map();
      seen.set(value, out);
      for (const [k, v] of value) out.set(cloneValue(k, seen), cloneValue(v, seen));
      return out;
    }
    if (value instanceof Set) {
      const out = new Set();
      seen.set(value, out);
      for (const item of value) out.add(cloneValue(item, seen));
      return out;
    }
    if (value instanceof Error) {
      const Ctor = value.constructor || Error;
      const out = new Ctor(value.message);
      out.name = value.name;
      return out;
    }
    if (Array.isArray(value)) {
      const out = [];
      seen.set(value, out);
      for (const item of value) out.push(cloneValue(item, seen));
      return out;
    }
    if (value instanceof ArrayBuffer) return value.slice(0);
    const out = {};
    seen.set(value, out);
    for (const [key, val] of Object.entries(value)) out[key] = cloneValue(val, seen);
    return out;
  }

  class VixenMessageEvent extends VixenEvent {
    constructor(type, init = {}) {
      super(type, init);
      const opts = init || {};
      defineReadonly(this, 'data', Object.prototype.hasOwnProperty.call(opts, 'data') ? opts.data : null, true);
      defineReadonly(this, 'origin', opts.origin === undefined ? '' : String(opts.origin), true);
      defineReadonly(this, 'lastEventId', opts.lastEventId === undefined ? '' : String(opts.lastEventId), true);
      defineReadonly(this, 'source', opts.source || null, true);
      defineReadonly(this, 'ports', Array.from(opts.ports || []), true);
    }
  }

  class VixenProgressEvent extends VixenEvent {
    constructor(type, init = {}) {
      super(type, init);
      const opts = init || {};
      defineReadonly(this, 'lengthComputable', Boolean(opts.lengthComputable), true);
      defineReadonly(this, 'loaded', Number(opts.loaded) || 0, true);
      defineReadonly(this, 'total', Number(opts.total) || 0, true);
    }
  }

  class VixenErrorEvent extends VixenEvent {
    constructor(type, init = {}) {
      super(type, init);
      const opts = init || {};
      defineReadonly(this, 'message', opts.message === undefined ? '' : String(opts.message), true);
      defineReadonly(this, 'filename', opts.filename === undefined ? '' : String(opts.filename), true);
      defineReadonly(this, 'lineno', Number(opts.lineno) || 0, true);
      defineReadonly(this, 'colno', Number(opts.colno) || 0, true);
      defineReadonly(this, 'error', Object.prototype.hasOwnProperty.call(opts, 'error') ? opts.error : null, true);
    }
  }

  class VixenCloseEvent extends VixenEvent {
    constructor(type, init = {}) {
      super(type, init);
      const opts = init || {};
      defineReadonly(this, 'wasClean', Boolean(opts.wasClean), true);
      defineReadonly(this, 'code', Number(opts.code) || 0, true);
      defineReadonly(this, 'reason', opts.reason === undefined ? '' : String(opts.reason), true);
    }
  }

  class VixenMessagePort extends VixenEventTarget {
    constructor() {
      super();
      defineData(this, '__vixenEntangledPort', null, false);
      defineData(this, '__vixenClosed', false, false);
      defineData(this, 'onmessage', null, true);
      defineData(this, 'onmessageerror', null, true);
    }
    postMessage(message, transfer = []) {
      if (this.__vixenClosed || !this.__vixenEntangledPort || this.__vixenEntangledPort.__vixenClosed) return;
      const target = this.__vixenEntangledPort;
      const payload = cloneValue(message);
      const ports = Array.from(transfer || []);
      queueMicrotaskCompat(() => {
        if (target.__vixenClosed) return;
        target.dispatchEvent(new VixenMessageEvent('message', { data: payload, ports }));
      });
    }
    start() {}
    close() {
      this.__vixenClosed = true;
      this.__vixenEntangledPort = null;
    }
  }

  class VixenMessageChannel {
    constructor() {
      const port1 = new VixenMessagePort();
      const port2 = new VixenMessagePort();
      port1.__vixenEntangledPort = port2;
      port2.__vixenEntangledPort = port1;
      defineReadonly(this, 'port1', port1, true);
      defineReadonly(this, 'port2', port2, true);
    }
  }

  const broadcastChannels = new Map();

  class VixenBroadcastChannel extends VixenEventTarget {
    constructor(name) {
      super();
      const channelName = String(name);
      defineReadonly(this, 'name', channelName, true);
      defineData(this, '__vixenClosed', false, false);
      defineData(this, 'onmessage', null, true);
      defineData(this, 'onmessageerror', null, true);
      if (!broadcastChannels.has(channelName)) broadcastChannels.set(channelName, new Set());
      broadcastChannels.get(channelName).add(this);
    }
    postMessage(message) {
      if (this.__vixenClosed) throw new TypeError('BroadcastChannel is closed');
      const peers = broadcastChannels.get(this.name) || new Set();
      const payload = cloneValue(message);
      for (const peer of peers) {
        if (peer === this || peer.__vixenClosed) continue;
        queueMicrotaskCompat(() => {
          if (!peer.__vixenClosed) peer.dispatchEvent(new VixenMessageEvent('message', { data: cloneValue(payload) }));
        });
      }
    }
    close() {
      this.__vixenClosed = true;
      const peers = broadcastChannels.get(this.name);
      if (peers) peers.delete(this);
    }
  }

  function targetRect(target) {
    if (target && typeof target.getBoundingClientRect === 'function') {
      try {
        const rect = target.getBoundingClientRect();
        return new VixenDOMRectReadOnly(rect.x, rect.y, rect.width, rect.height);
      } catch (_) {}
    }
    return new VixenDOMRectReadOnly(0, 0, 0, 0);
  }

  function viewportRect() {
    return new VixenDOMRectReadOnly(0, 0, Number(globalThis.innerWidth) || 800, Number(globalThis.innerHeight) || 600);
  }

  function observerThresholds(options) {
    const raw = options && Object.prototype.hasOwnProperty.call(options, 'threshold') ? options.threshold : 0;
    const values = Array.isArray(raw) ? raw : [raw];
    const thresholds = values.map((value) => finiteNumber(value, 0)).filter((value) => value >= 0 && value <= 1);
    return thresholds.length === 0 ? [0] : Array.from(new Set(thresholds)).sort((a, b) => a - b);
  }

  class VixenIntersectionObserverEntry {
    constructor(init = {}) {
      defineReadonly(this, 'time', finiteNumber(init.time, performance.now()), true);
      defineReadonly(this, 'rootBounds', init.rootBounds || null, true);
      defineReadonly(this, 'boundingClientRect', init.boundingClientRect || new VixenDOMRectReadOnly(), true);
      defineReadonly(this, 'intersectionRect', init.intersectionRect || new VixenDOMRectReadOnly(), true);
      defineReadonly(this, 'isIntersecting', Boolean(init.isIntersecting), true);
      defineReadonly(this, 'intersectionRatio', finiteNumber(init.intersectionRatio, 0), true);
      defineReadonly(this, 'target', init.target || null, true);
    }
  }

  class VixenIntersectionObserver {
    constructor(callback, options = {}) {
      if (typeof callback !== 'function') throw new TypeError('IntersectionObserver callback must be a function');
      defineReadonly(this, '__vixenCallback', callback, false);
      defineReadonly(this, '__vixenRecords', [], false);
      defineReadonly(this, '__vixenTargets', new Set(), false);
      defineReadonly(this, 'root', options && options.root ? options.root : null, true);
      defineReadonly(this, 'rootMargin', options && options.rootMargin !== undefined ? String(options.rootMargin) : '0px', true);
      defineReadonly(this, 'thresholds', observerThresholds(options || {}), true);
      defineReadonly(this, 'scrollMargin', '0px', true);
      defineReadonly(this, 'delay', 0, true);
      defineReadonly(this, 'trackVisibility', false, true);
    }
    observe(target) {
      if (target === null || (typeof target !== 'object' && typeof target !== 'function')) throw new TypeError('IntersectionObserver.observe target must be an Element');
      this.__vixenTargets.add(target);
      const rect = targetRect(target);
      const intersects = rect.width > 0 && rect.height > 0;
      this.__vixenRecords.push(new VixenIntersectionObserverEntry({
        time: performance.now(),
        rootBounds: viewportRect(),
        boundingClientRect: rect,
        intersectionRect: intersects ? rect : new VixenDOMRectReadOnly(0, 0, 0, 0),
        isIntersecting: intersects,
        intersectionRatio: intersects ? 1 : 0,
        target,
      }));
      queueMicrotaskCompat(() => this.__vixenDeliver());
    }
    unobserve(target) { this.__vixenTargets.delete(target); }
    disconnect() { this.__vixenTargets.clear(); this.__vixenRecords.splice(0, this.__vixenRecords.length); }
    takeRecords() { return this.__vixenRecords.splice(0, this.__vixenRecords.length); }
    __vixenDeliver() {
      const records = this.takeRecords();
      if (records.length > 0) this.__vixenCallback.call(this, records, this);
    }
  }

  class VixenResizeObserverSize {
    constructor(inlineSize = 0, blockSize = 0) {
      defineReadonly(this, 'inlineSize', finiteNumber(inlineSize, 0), true);
      defineReadonly(this, 'blockSize', finiteNumber(blockSize, 0), true);
    }
  }

  class VixenResizeObserverEntry {
    constructor(target) {
      const rect = targetRect(target);
      const size = new VixenResizeObserverSize(rect.width, rect.height);
      defineReadonly(this, 'target', target, true);
      defineReadonly(this, 'contentRect', rect, true);
      defineReadonly(this, 'borderBoxSize', [size], true);
      defineReadonly(this, 'contentBoxSize', [size], true);
      defineReadonly(this, 'devicePixelContentBoxSize', [size], true);
    }
  }

  class VixenResizeObserver {
    constructor(callback) {
      if (typeof callback !== 'function') throw new TypeError('ResizeObserver callback must be a function');
      defineReadonly(this, '__vixenCallback', callback, false);
      defineReadonly(this, '__vixenTargets', new Set(), false);
      defineReadonly(this, '__vixenRecords', [], false);
    }
    observe(target) {
      if (target === null || (typeof target !== 'object' && typeof target !== 'function')) throw new TypeError('ResizeObserver.observe target must be an Element');
      this.__vixenTargets.add(target);
      this.__vixenRecords.push(new VixenResizeObserverEntry(target));
      queueMicrotaskCompat(() => this.__vixenDeliver());
    }
    unobserve(target) { this.__vixenTargets.delete(target); }
    disconnect() { this.__vixenTargets.clear(); this.__vixenRecords.splice(0, this.__vixenRecords.length); }
    __vixenDeliver() {
      const records = this.__vixenRecords.splice(0, this.__vixenRecords.length);
      if (records.length > 0) this.__vixenCallback.call(this, records, this);
    }
  }

  class VixenClipboardItem {
    constructor(items = {}, options = {}) {
      defineReadonly(this, '__vixenItems', new Map(), false);
      for (const [type, value] of Object.entries(items || {})) {
        const key = String(type).toLowerCase();
        this.__vixenItems.set(key, Promise.resolve(value).then((resolved) => resolved instanceof VixenBlob ? resolved : new VixenBlob([resolved], { type: key })));
      }
      defineReadonly(this, 'presentationStyle', options && options.presentationStyle !== undefined ? String(options.presentationStyle) : 'unspecified', true);
    }
    get types() { return Array.from(this.__vixenItems.keys()); }
    getType(type) {
      const key = String(type).toLowerCase();
      if (!this.__vixenItems.has(key)) return Promise.reject(new TypeError('ClipboardItem type not found'));
      return this.__vixenItems.get(key);
    }
    static supports(type) { return String(type).toLowerCase() === 'text/plain'; }
  }

  let clipboardText = '';

  class VixenClipboard extends VixenEventTarget {
    readText() { return Promise.resolve(clipboardText); }
    writeText(text) { clipboardText = String(text); return Promise.resolve(); }
    read() { return Promise.resolve([new VixenClipboardItem({ 'text/plain': new VixenBlob([clipboardText], { type: 'text/plain' }) })]); }
    write(items) {
      const first = Array.from(items || [])[0];
      if (!(first instanceof VixenClipboardItem)) return Promise.reject(new TypeError('Clipboard.write expects ClipboardItem values'));
      return first.getType('text/plain').then((blob) => blob.text()).then((text) => { clipboardText = text; });
    }
  }

  function integerTypedArray(value) {
    const constructors = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array];
    if (typeof BigInt64Array === 'function') constructors.push(BigInt64Array);
    if (typeof BigUint64Array === 'function') constructors.push(BigUint64Array);
    return constructors.some((ctor) => value instanceof ctor);
  }

  function randomHex(bytes) {
    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
  }

  class VixenSubtleCrypto {}

  class VixenCrypto {
    constructor() { defineReadonly(this, 'subtle', new VixenSubtleCrypto(), true); }
    getRandomValues(array) {
      if (!integerTypedArray(array)) throw new TypeError('Crypto.getRandomValues requires an integer typed array');
      if (array.byteLength > 65_536) throw new TypeError('Crypto.getRandomValues quota exceeded');
      const result = op_vixen_crypto_random_bytes(array.byteLength);
      if (!result || !result.ok) throw new TypeError(result && result.message ? result.message : 'secure random source unavailable');
      new Uint8Array(array.buffer, array.byteOffset, array.byteLength).set(result.bytes);
      return array;
    }
    randomUUID() {
      const bytes = new Uint8Array(16);
      this.getRandomValues(bytes);
      bytes[6] = (bytes[6] & 0x0f) | 0x40;
      bytes[8] = (bytes[8] & 0x3f) | 0x80;
      const hex = randomHex(bytes);
      return hex.slice(0, 8) + '-' + hex.slice(8, 12) + '-' + hex.slice(12, 16) + '-' + hex.slice(16, 20) + '-' + hex.slice(20);
    }
  }

  class VixenWebSocket extends VixenEventTarget {
    constructor(url, protocols = []) {
      super();
      const parsed = new VixenURL(String(url), typeof location !== 'undefined' ? location.href : undefined);
      if (parsed.protocol !== 'ws:' && parsed.protocol !== 'wss:') throw new TypeError('WebSocket URL must use ws: or wss:');
      defineReadonly(this, 'url', parsed.href, true);
      defineData(this, 'readyState', VixenWebSocket.CONNECTING, true);
      defineReadonly(this, 'bufferedAmount', 0, true);
      defineData(this, 'extensions', '', true);
      defineData(this, 'protocol', Array.isArray(protocols) ? String(protocols[0] || '') : String(protocols || ''), true);
      defineData(this, 'binaryType', 'blob', true);
      defineData(this, 'onopen', null, true);
      defineData(this, 'onmessage', null, true);
      defineData(this, 'onerror', null, true);
      defineData(this, 'onclose', null, true);
      queueMicrotaskCompat(() => {
        if (this.readyState !== VixenWebSocket.CONNECTING) return;
        this.readyState = VixenWebSocket.CLOSED;
        this.dispatchEvent(new VixenErrorEvent('error', { message: 'WebSocket network connections are not implemented by Vixen yet' }));
        this.dispatchEvent(new VixenCloseEvent('close', { wasClean: false, code: 1006, reason: 'unsupported' }));
      });
    }
    send(_data) {
      if (this.readyState !== VixenWebSocket.OPEN) throw new TypeError('WebSocket is not open');
    }
    close(code = 1000, reason = '') {
      if (this.readyState === VixenWebSocket.CLOSED) return;
      this.readyState = VixenWebSocket.CLOSED;
      this.dispatchEvent(new VixenCloseEvent('close', { wasClean: true, code: Number(code) || 1000, reason: String(reason) }));
    }
  }

  for (const [name, value] of [['CONNECTING', 0], ['OPEN', 1], ['CLOSING', 2], ['CLOSED', 3]]) {
    defineReadonly(VixenWebSocket, name, value, true);
    defineReadonly(VixenWebSocket.prototype, name, value, true);
  }

  class VixenDOMParser {
    parseFromString(source, _type) { return new VixenParsedDocument(String(source)); }
  }

  class VixenParsedElement {
    constructor(tag, attrs, html) { this.localName = tag; this.tagName = tag.toUpperCase(); this.__vixenAttrs = attrs; this.innerHTML = html; }
    get id() { return this.__vixenAttrs.id || ''; }
    get textContent() { return this.innerHTML.replace(/<[^>]*>/g, ''); }
    getAttribute(name) { return Object.prototype.hasOwnProperty.call(this.__vixenAttrs, name) ? this.__vixenAttrs[name] : null; }
  }

  class VixenParsedDocument {
    constructor(source) { this.__vixenSource = source; }
    querySelector(selector) {
      const source = this.__vixenSource;
      if (String(selector).startsWith('#')) {
        const id = String(selector).slice(1).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        const re = new RegExp("<([A-Za-z][A-Za-z0-9-]*)([^>]*)\\bid=[\"']" + id + "[\"']([^>]*)>([\\s\\S]*?)<\\/\\1>", 'i');
        const m = source.match(re);
        if (!m) return null;
        return new VixenParsedElement(m[1].toLowerCase(), parseAttrs(m[2] + ' ' + m[3]), m[4]);
      }
      const tag = String(selector).toLowerCase();
      const re = new RegExp('<(' + tag + ')([^>]*)>([\\s\\S]*?)<\\/\\1>', 'i');
      const m = source.match(re);
      return m ? new VixenParsedElement(m[1].toLowerCase(), parseAttrs(m[2]), m[3]) : null;
    }
  }

  function parseAttrs(raw) {
    const attrs = {};
    raw.replace(/([A-Za-z_:][-A-Za-z0-9_:.]*)\s*=\s*(["'])(.*?)\2/g, (_m, name, _q, value) => { attrs[name] = value; return ''; });
    return attrs;
  }

  webidl.adoptInterface('AbortSignal', VixenAbortSignal);
  webidl.adoptInterface('AbortController', VixenAbortController);
  webidl.adoptInterface('MutationRecord', VixenMutationRecord);
  webidl.adoptInterface('MutationObserver', VixenMutationObserver);
  webidl.adoptInterface('MessageEvent', VixenMessageEvent);
  webidl.adoptInterface('ProgressEvent', VixenProgressEvent);
  webidl.adoptInterface('ErrorEvent', VixenErrorEvent);
  webidl.adoptInterface('CloseEvent', VixenCloseEvent);
  webidl.adoptInterface('MessagePort', VixenMessagePort);
  webidl.adoptInterface('MessageChannel', VixenMessageChannel);
  webidl.adoptInterface('BroadcastChannel', VixenBroadcastChannel);
  webidl.adoptInterface('IntersectionObserverEntry', VixenIntersectionObserverEntry);
  webidl.adoptInterface('IntersectionObserver', VixenIntersectionObserver);
  webidl.adoptInterface('ResizeObserverSize', VixenResizeObserverSize);
  webidl.adoptInterface('ResizeObserverEntry', VixenResizeObserverEntry);
  webidl.adoptInterface('ResizeObserver', VixenResizeObserver);
  webidl.adoptInterface('ClipboardItem', VixenClipboardItem);
  webidl.adoptInterface('Clipboard', VixenClipboard);
  webidl.adoptInterface('SubtleCrypto', VixenSubtleCrypto);
  webidl.adoptInterface('Crypto', VixenCrypto);
  webidl.adoptInterface('WebSocket', VixenWebSocket);
  webidl.adoptInterface('DOMParser', VixenDOMParser);

  defineGlobal('__vixenQueueMutationRecord', queueMutationRecord);

  defineGlobal('structuredClone', cloneValue);

  const base64Alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  defineGlobal('btoa', (input) => {
    const bytes = Array.from(String(input), (ch) => ch.charCodeAt(0) & 0xff);
    let out = '';
    for (let i = 0; i < bytes.length; i += 3) {
      const n = (bytes[i] << 16) | ((bytes[i + 1] || 0) << 8) | (bytes[i + 2] || 0);
      out += base64Alphabet[(n >> 18) & 63] + base64Alphabet[(n >> 12) & 63] + (i + 1 < bytes.length ? base64Alphabet[(n >> 6) & 63] : '=') + (i + 2 < bytes.length ? base64Alphabet[n & 63] : '=');
    }
    return out;
  });
  defineGlobal('atob', (input) => {
    const clean = String(input).replace(/=+$/, '');
    let bits = 0, bitLength = 0, out = '';
    for (const ch of clean) {
      const value = base64Alphabet.indexOf(ch);
      if (value < 0) throw new TypeError('invalid base64');
      bits = (bits << 6) | value;
      bitLength += 6;
      if (bitLength >= 8) {
        bitLength -= 8;
        out += String.fromCharCode((bits >> bitLength) & 0xff);
      }
    }
    return out;
  });

  class VixenPerformance extends VixenEventTarget {
    get timeOrigin() { return startEpoch; }
    now() { return Math.max(0, Date.now() - startEpoch); }
    toJSON() { return { timeOrigin: this.timeOrigin }; }
    mark() {}
    measure() {}
    clearMarks() {}
    clearMeasures() {}
    getEntries() { return []; }
    getEntriesByName() { return []; }
    getEntriesByType() { return []; }
  }

  function unwrapStorageOp(result) {
    if (!result.ok) {
      const error = new TypeError(result.message);
      if (result.name) error.name = result.name;
      throw error;
    }
    return result;
  }

  function storageIndex(index) {
    return Number(index) >>> 0;
  }

  class VixenStorage {
    constructor(kind = 'local') {
      defineReadonly(this, '__vixenKind', kind, false);
    }
    get length() { return op_vixen_storage_length(this.__vixenKind); }
    key(index = 0) {
      return unwrapStorageOp(op_vixen_storage_key(this.__vixenKind, storageIndex(index))).value;
    }
    getItem(key) {
      return unwrapStorageOp(op_vixen_storage_get(this.__vixenKind, String(key))).value;
    }
    setItem(key, value) {
      unwrapStorageOp(op_vixen_storage_set(this.__vixenKind, String(key), String(value)));
    }
    removeItem(key) {
      unwrapStorageOp(op_vixen_storage_remove(this.__vixenKind, String(key)));
    }
    clear() {
      unwrapStorageOp(op_vixen_storage_clear(this.__vixenKind));
    }
  }

  class VixenStorageManager {
    estimate() {
      return Promise.resolve().then(() => {
        const result = unwrapStorageOp(op_vixen_storage_estimate());
        return { usage: Number(result.usage) || 0, quota: Number(result.quota) || 0 };
      });
    }
    persisted() { return Promise.resolve(Boolean(op_vixen_storage_persisted())); }
    persist() { return this.persisted(); }
  }

  function unwrapPermissionOp(result) {
    if (!result || !result.ok) throw new TypeError(result && result.message ? result.message : 'permission query failed');
    return result;
  }

  class VixenPermissionStatus extends EventTarget {
    constructor(name, state) {
      super();
      this.__vixenName = String(name);
      this.__vixenState = String(state);
      this.onchange = null;
    }
    get state() { return this.__vixenState; }
    get onchange() { return this.__vixenOnchange || null; }
    set onchange(value) { this.__vixenOnchange = value; }
  }

  class VixenPermissions {
    query(descriptor = {}) {
      return Promise.resolve().then(() => {
        if (!descriptor || descriptor.name === undefined) throw new TypeError('permission descriptor.name is required');
        const name = String(descriptor.name);
        const result = unwrapPermissionOp(op_vixen_permission_query(name));
        return new VixenPermissionStatus(name, result.state);
      });
    }
  }

  function notificationPermission() {
    const state = unwrapPermissionOp(op_vixen_permission_query('notifications')).state;
    return state === 'prompt' ? 'default' : state;
  }

  class VixenNotification extends EventTarget {
    constructor(title = '', options = {}) {
      super();
      if (VixenNotification.permission !== 'granted') throw new TypeError('Notification permission is not granted');
      this.title = String(title);
      this.body = String(options && options.body !== undefined ? options.body : '');
    }
    static get permission() { return notificationPermission(); }
    static requestPermission(callback = undefined) {
      const permission = notificationPermission();
      if (typeof callback === 'function') Promise.resolve().then(() => callback(permission));
      return Promise.resolve(permission);
    }
    close() {}
  }

  class VixenNavigator {
    constructor() {
      this.__vixenPermissions = new VixenPermissions();
      this.__vixenStorageManager = new VixenStorageManager();
      this.__vixenClipboard = new VixenClipboard();
    }
    get userAgent() { return 'Vixen/0.1'; }
    get language() { return 'en-US'; }
    get languages() { return ['en-US']; }
    get onLine() { return true; }
    get cookieEnabled() { return true; }
    get hardwareConcurrency() { return 1; }
    get maxTouchPoints() { return 0; }
    get permissions() { return this.__vixenPermissions; }
    get storage() { return this.__vixenStorageManager; }
    get clipboard() { return this.__vixenClipboard; }
    sendBeacon() { return false; }
    vibrate() { return false; }
  }

  function mediaMatches(query) {
    const q = String(query).toLowerCase();
    const width = Number(globalThis.innerWidth) || 800;
    const height = Number(globalThis.innerHeight) || 600;
    const media = globalThis.__vixenEmulatedMedia || {};
    const mediaType = String(media.media || 'screen').toLowerCase();
    const colorScheme = String(media.colorScheme || 'light').toLowerCase();
    if (q.includes('print') && mediaType !== 'print') return false;
    if (q.includes('screen') && mediaType === 'print') return false;
    const requestedColorScheme = q.match(/prefers-color-scheme:\s*(dark|light|no-preference)/);
    if (requestedColorScheme && colorScheme !== requestedColorScheme[1]) return false;
    if (q.includes('orientation: landscape')) return width >= height;
    if (q.includes('orientation: portrait')) return height > width;
    const min = q.match(/min-width:\s*(\d+)px/);
    if (min && width < Number(min[1])) return false;
    const max = q.match(/max-width:\s*(\d+)px/);
    if (max && width > Number(max[1])) return false;
    return q.includes('screen') || q.includes('print') || q.includes('min-width') || q.includes('max-width') || q.includes('prefers-color-scheme') || q.trim() === 'all';
  }

  function matchMedia(query) {
    return { media: String(query), matches: mediaMatches(query), onchange: null, addEventListener() {}, removeEventListener() {}, dispatchEvent() { return true; } };
  }

  let nextTimerId = 1;
  const documentTasks = new Map();
  function scheduleDocumentTask(callback, timeout, args, interval, animationFrame) {
    const id = nextTimerId++;
    const delay = Math.max(0, Number(timeout) || 0);
    documentTasks.set(id, {
      callback,
      args,
      delay,
      due: performance.now() + delay,
      interval,
      animationFrame,
    });
    return id;
  }
  function setTimeoutShim(callback, timeout = 0, ...args) {
    return scheduleDocumentTask(callback, timeout, args, false, false);
  }
  function clearTimeoutShim(id) { documentTasks.delete(Number(id)); }
  function setIntervalShim(callback, timeout = 0, ...args) {
    return scheduleDocumentTask(callback, timeout, args, true, false);
  }
  function clearIntervalShim(id) { clearTimeoutShim(id); }
  function requestAnimationFrameShim(callback) {
    if (typeof callback !== 'function') throw new TypeError('requestAnimationFrame callback must be a function');
    return scheduleDocumentTask(callback, 0, [], false, true);
  }
  function cancelAnimationFrameShim(id) { clearTimeoutShim(id); }

  function readyDocumentTaskIds(limit = 64) {
    const maximum = Math.min(64, Math.max(0, Number(limit) || 0));
    const now = performance.now();
    const ready = [];
    for (const [id, task] of documentTasks) {
      if (task.due <= now) ready.push(id);
      if (ready.length >= maximum) break;
    }
    return ready;
  }

  function runDocumentTask(id) {
    const taskId = Number(id);
    const task = documentTasks.get(taskId);
    if (!task) return false;
    if (task.interval) task.due = performance.now() + Math.max(1, task.delay);
    else documentTasks.delete(taskId);
    if (task.animationFrame) task.callback(performance.now());
    else if (typeof task.callback === 'function') task.callback(...task.args);
    else globalThis.eval(String(task.callback));
    return true;
  }

  webidl.adoptInterface('Performance', VixenPerformance);
  webidl.adoptInterface('Storage', VixenStorage);
  webidl.adoptInterface('StorageManager', VixenStorageManager);
  webidl.adoptInterface('PermissionStatus', VixenPermissionStatus);
  webidl.adoptInterface('Permissions', VixenPermissions);
  webidl.adoptInterface('Notification', VixenNotification);
  webidl.adoptInterface('Navigator', VixenNavigator);

  if (typeof globalThis.window === 'undefined') defineGlobal('window', globalThis);
  if (typeof globalThis.self === 'undefined') defineGlobal('self', globalThis);
  defineGlobal('performance', new VixenPerformance());
  defineGlobal('navigator', new VixenNavigator());
  defineGlobal('crypto', new VixenCrypto());
  defineGlobal('localStorage', new VixenStorage('local'));
  defineGlobal('sessionStorage', new VixenStorage('session'));
  defineGlobal('screen', { width: 800, height: 600, availWidth: 800, availHeight: 600, colorDepth: 24, pixelDepth: 24 });
  defineGlobal('visualViewport', { offsetLeft: 0, offsetTop: 0, pageLeft: 0, pageTop: 0, width: 800, height: 600, scale: 1 });
  defineGlobal('matchMedia', matchMedia);
  defineGlobal('setTimeout', setTimeoutShim);
  defineGlobal('clearTimeout', clearTimeoutShim);
  defineGlobal('setInterval', setIntervalShim);
  defineGlobal('clearInterval', clearIntervalShim);
  defineGlobal('requestAnimationFrame', requestAnimationFrameShim);
  defineGlobal('cancelAnimationFrame', cancelAnimationFrameShim);
  defineGlobal('__vixenReadyDocumentTaskIds', readyDocumentTaskIds);
  defineGlobal('__vixenRunDocumentTask', runDocumentTask);
  Object.defineProperties(globalThis, {
    innerWidth: { value: 800, writable: true, configurable: true },
    innerHeight: { value: 600, writable: true, configurable: true },
    devicePixelRatio: { value: 1, writable: true, configurable: true },
  });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn webapi_bootstrap_is_ascii_and_adopts_runtime_interfaces() {
        assert!(WEB_API_BOOTSTRAP.is_ascii());
        assert!(WEB_API_BOOTSTRAP.contains("adoptInterface('Event'"));
        assert!(WEB_API_BOOTSTRAP.contains("adoptInterface('DOMMatrix'"));
        assert!(WEB_API_BOOTSTRAP.contains("adoptInterface('Headers'"));
        assert!(WEB_API_BOOTSTRAP.contains("op_vixen_storage_set"));
        assert!(WEB_API_BOOTSTRAP.contains("structuredClone"));
    }

    #[test]
    fn preflight_cache_is_bounded_partitioned_and_expires() {
        let now = Instant::now();
        let key = PreflightCacheKey {
            request_origin: "https://app.example".to_owned(),
            target_origin: "https://api.example".to_owned(),
            credentials_mode: CorsCredentialsMode::Omit,
        };
        let mut cache = PreflightCache::default();
        cache.insert(PreflightCacheEntry {
            key: key.clone(),
            allow_methods: vec!["post".to_owned()],
            allow_headers: vec!["x-vixen".to_owned()],
            expires_at: now + Duration::from_secs(30),
        });

        assert!(cache.allows(&key, Method::Post, &["x-vixen".to_owned()], now));
        assert!(!cache.allows(&key, Method::Put, &["x-vixen".to_owned()], now));
        assert!(!cache.allows(
            &PreflightCacheKey {
                request_origin: "https://other.example".to_owned(),
                ..key.clone()
            },
            Method::Post,
            &["x-vixen".to_owned()],
            now,
        ));
        assert!(!cache.allows(
            &key,
            Method::Post,
            &["x-vixen".to_owned()],
            now + Duration::from_secs(31),
        ));

        for index in 0..=MAX_PREFLIGHT_CACHE_ENTRIES {
            cache.insert(PreflightCacheEntry {
                key: PreflightCacheKey {
                    request_origin: format!("https://{index}.example"),
                    ..key.clone()
                },
                allow_methods: vec!["post".to_owned()],
                allow_headers: Vec::new(),
                expires_at: now + Duration::from_secs(30),
            });
        }
        assert_eq!(cache.entries.len(), MAX_PREFLIGHT_CACHE_ENTRIES);
    }

    #[test]
    fn fetch_integrity_accepts_match_and_rejects_mismatch() {
        let response = TextResponse {
            body: "abc".to_owned(),
            headers: BTreeMap::new(),
            status: 200,
            final_url: "https://cdn.example/app.js".to_owned(),
            set_cookie: Vec::new(),
            redirects: 0,
            events: Vec::new(),
            request_headers: BTreeMap::new(),
        };
        let matching = "sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";

        assert!(apply_fetch_integrity(response.clone(), matching).is_ok());
        assert_eq!(
            apply_fetch_integrity(response, "sha256-AAAA").unwrap_err(),
            "fetch blocked by integrity mismatch (sha256)"
        );
    }
}
