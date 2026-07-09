//! JavaScript-only browser value-object compatibility layer.
//!
//! These bindings deliberately stay in `deno_core`/V8: they fill WebIDL-shaped
//! constructor/prototype behavior for pure value APIs before a backend is
//! involved. Page-backed DOM objects live in `script::dom`.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use deno_core::serde_json::{Value, json};
use deno_core::{Extension, ExtensionFileSource, OpState};
use url::Url;
use vixen_net::{CookieJar, Method, Network, NetworkConfig, TextResponse, validate_http_url};
use vixen_store::Store;

use crate::storage_key::{
    MAX_PARTITION_BYTES, StorageKeyError, StorageKind, StorageQuota, validate_storage_key,
    validate_storage_value,
};

struct WebApiHost {
    storage: WebStorageHost,
    network: Result<Network, String>,
}

type StorageEntries = Vec<(String, String)>;
type MemoryStorageMap = Arc<Mutex<HashMap<String, StorageEntries>>>;

impl WebApiHost {
    fn new(network_config: NetworkConfig, storage: WebStorageHost) -> Self {
        Self {
            storage,
            network: Network::new(network_config).map_err(|err| err.to_string()),
        }
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
        op_vixen_fetch,
    ],
);

pub(super) fn extension(network_config: NetworkConfig, storage: WebStorageHost) -> Extension {
    let mut extension = vixen_webapi::init();
    extension.op_state_fn = Some(Box::new(move |state| {
        state.put(WebApiHost::new(network_config, storage.clone()));
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
fn op_vixen_fetch(
    state: Rc<RefCell<OpState>>,
    #[serde] request: deno_core::serde_json::Value,
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
    let url = match Url::parse(url_text) {
        Ok(url) => url,
        Err(err) => return fetch_error(format!("invalid URL: {err}")),
    };
    if let Err(err) = validate_http_url(&url) {
        return fetch_error(format!("URL rejected by policy: {err}"));
    }

    let network = {
        let state = state.borrow();
        let host = state.borrow::<WebApiHost>();
        match &host.network {
            Ok(network) => network.clone(),
            Err(message) => return fetch_error(format!("network unavailable: {message}")),
        }
    };

    match fetch_http_text_blocking(network, url, method) {
        Ok(response) => fetch_response(response),
        Err(message) => fetch_error(message),
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

fn fetch_http_text_blocking(
    network: Network,
    url: Url,
    method: Method,
) -> Result<TextResponse, String> {
    let handle = std::thread::Builder::new()
        .name("vixen-fetch".to_owned())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| format!("network runtime unavailable: {err}"))?;
            let mut network = network;
            let mut jar = CookieJar::default();
            rt.block_on(network.get_text_with_cookies(&mut jar, &url, false, method))
                .map_err(|err| err.to_string())
        })
        .map_err(|err| format!("fetch worker spawn failed: {err}"))?;
    handle
        .join()
        .map_err(|_| "fetch worker panicked".to_owned())?
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

fn fetch_response(response: TextResponse) -> deno_core::serde_json::Value {
    json!({
        "ok": true,
        "body": response.body,
        "headers": response.headers,
        "status": response.status,
        "finalUrl": response.final_url,
        "redirected": response.redirects > 0,
    })
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
    op_vixen_fetch,
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
    stream() { return null; }
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
    return { isNull: false, contentType: 'text/plain;charset=UTF-8', text: String(body) };
  }

  class VixenRequest {
    constructor(input, init = {}) {
      const source = input instanceof VixenRequest ? input : null;
      const url = source ? source.url : new VixenURL(String(input)).href;
      const method = String((init && init.method) || (source && source.method) || 'GET').toUpperCase();
      const body = bodyInfo(init && Object.prototype.hasOwnProperty.call(init, 'body') ? init.body : null);
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
      defineReadonly(this, 'credentials', (init && init.credentials) || 'same-origin', true);
      defineReadonly(this, 'cache', (init && init.cache) || 'default', true);
      defineReadonly(this, 'redirect', (init && init.redirect) || 'follow', true);
      defineReadonly(this, 'integrity', (init && init.integrity) || '', true);
      defineReadonly(this, 'keepalive', Boolean(init && init.keepalive), true);
      defineReadonly(this, 'signal', (init && init.signal) || new VixenAbortController().signal, true);
      defineReadonly(this, '__vixenBodyText', body.text, false);
      defineReadonly(this, 'body', body.isNull ? null : {}, true);
      defineReadonly(this, 'bodyUsed', false, true);
    }
    clone() { return new VixenRequest(this); }
    text() { return Promise.resolve(this.__vixenBodyText); }
    json() { return Promise.resolve(this.__vixenBodyText === '' ? null : JSON.parse(this.__vixenBodyText)); }
    blob() { return Promise.resolve(new VixenBlob([this.__vixenBodyText])); }
    arrayBuffer() { return Promise.resolve(textEncoder.encode(this.__vixenBodyText).buffer); }
    bytes() { return Promise.resolve(textEncoder.encode(this.__vixenBodyText)); }
    formData() { return Promise.resolve(new FormData()); }
  }

  class VixenResponse {
    constructor(body = null, init = {}) {
      const info = bodyInfo(body);
      const status = init && init.status !== undefined ? finiteNumber(init.status, 200) : 200;
      const headers = filteredHeaders(init && init.headers, forbiddenResponseHeader);
      if (!info.isNull && info.contentType && !headers.has('content-type')) headers.set('Content-Type', info.contentType);
      defineReadonly(this, 'type', 'default', true);
      defineReadonly(this, 'url', (init && init.url) || '', true);
      defineReadonly(this, 'redirected', Boolean(init && init.redirected), true);
      defineReadonly(this, 'status', status, true);
      defineReadonly(this, 'ok', status >= 200 && status <= 299, true);
      defineReadonly(this, 'statusText', (init && init.statusText) || '', true);
      defineReadonly(this, 'headers', headers, true);
      defineReadonly(this, '__vixenBodyText', info.text, false);
      defineReadonly(this, 'body', info.isNull ? null : {}, true);
      defineReadonly(this, 'bodyUsed', false, true);
    }
    clone() { return new VixenResponse(this.__vixenBodyText, { status: this.status, statusText: this.statusText, headers: this.headers, url: this.url, redirected: this.redirected }); }
    text() { return Promise.resolve(this.__vixenBodyText); }
    json() { return Promise.resolve(this.__vixenBodyText === '' ? null : JSON.parse(this.__vixenBodyText)); }
    blob() { return Promise.resolve(new VixenBlob([this.__vixenBodyText], { type: this.headers.get('content-type') || '' })); }
    arrayBuffer() { return Promise.resolve(textEncoder.encode(this.__vixenBodyText).buffer); }
    bytes() { return Promise.resolve(textEncoder.encode(this.__vixenBodyText)); }
    formData() { return Promise.resolve(new FormData()); }
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

  function fetch(input, init = {}) {
    let request;
    try {
      request = new VixenRequest(input, init);
    } catch (err) {
      return Promise.reject(err);
    }
    const result = op_vixen_fetch({ url: request.url, method: request.method });
    if (!result || !result.ok) {
      return Promise.reject(new TypeError(result && result.message ? result.message : 'fetch failed'));
    }
    return Promise.resolve(new VixenResponse(result.body, {
      status: result.status,
      headers: result.headers,
      url: result.finalUrl,
      redirected: result.redirected,
    }));
  }

  defineGlobal('fetch', fetch);

  // -----------------------------------------------------------------------
  // Abort, MutationObserver, structuredClone, DOMParser, platform globals
  // -----------------------------------------------------------------------

  class VixenAbortSignal extends VixenEventTarget {
    constructor(aborted = false, reason = undefined) { super(); defineReadonly(this, '__vixenAbortState', { aborted, reason }, false); }
    get aborted() { return this.__vixenAbortState.aborted; }
    get reason() { return this.__vixenAbortState.reason; }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    static abort(reason = undefined) { return new VixenAbortSignal(true, reason); }
    static timeout(_ms) { return new VixenAbortSignal(true, new Error('TimeoutError')); }
    static any(signals) { return new VixenAbortSignal(Array.from(signals).some((signal) => signal.aborted), undefined); }
  }

  class VixenAbortController {
    constructor() { defineReadonly(this, 'signal', new VixenAbortSignal(false), true); }
    abort(reason = undefined) { this.signal.__vixenAbortState.aborted = true; this.signal.__vixenAbortState.reason = reason; }
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

  class VixenNavigator {
    get userAgent() { return 'Vixen/0.1'; }
    get language() { return 'en-US'; }
    get languages() { return ['en-US']; }
    get onLine() { return true; }
    get cookieEnabled() { return true; }
    get hardwareConcurrency() { return 1; }
    get maxTouchPoints() { return 0; }
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
  function runSoon(callback, args) {
    const id = nextTimerId++;
    if (typeof callback === 'function') {
      Promise.resolve().then(() => callback(...args));
    } else {
      Promise.resolve().then(() => globalThis.eval(String(callback)));
    }
    return id;
  }
  function setTimeoutShim(callback, timeout = 0, ...args) { return runSoon(callback, args); }
  function clearTimeoutShim(id) {}
  function setIntervalShim(callback, timeout = 0, ...args) { return runSoon(callback, args); }
  function clearIntervalShim(id) {}
  function requestAnimationFrameShim(callback) {
    return runSoon((cb) => cb(performance.now()), [callback]);
  }
  function cancelAnimationFrameShim(id) {}

  webidl.adoptInterface('Performance', VixenPerformance);
  webidl.adoptInterface('Storage', VixenStorage);
  webidl.adoptInterface('Navigator', VixenNavigator);

  if (typeof globalThis.window === 'undefined') defineGlobal('window', globalThis);
  if (typeof globalThis.self === 'undefined') defineGlobal('self', globalThis);
  defineGlobal('performance', new VixenPerformance());
  defineGlobal('navigator', new VixenNavigator());
  defineGlobal('localStorage', new VixenStorage('local'));
  defineGlobal('sessionStorage', new VixenStorage('session'));
  defineGlobal('history', { length: 1, state: null, scrollRestoration: 'auto', go() {}, back() {}, forward() {}, pushState() {}, replaceState() {} });
  defineGlobal('screen', { width: 800, height: 600, availWidth: 800, availHeight: 600, colorDepth: 24, pixelDepth: 24 });
  defineGlobal('visualViewport', { offsetLeft: 0, offsetTop: 0, pageLeft: 0, pageTop: 0, width: 800, height: 600, scale: 1 });
  defineGlobal('matchMedia', matchMedia);
  defineGlobal('setTimeout', setTimeoutShim);
  defineGlobal('clearTimeout', clearTimeoutShim);
  defineGlobal('setInterval', setIntervalShim);
  defineGlobal('clearInterval', clearIntervalShim);
  defineGlobal('requestAnimationFrame', requestAnimationFrameShim);
  defineGlobal('cancelAnimationFrame', cancelAnimationFrameShim);
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

    #[test]
    fn webapi_bootstrap_is_ascii_and_adopts_runtime_interfaces() {
        assert!(WEB_API_BOOTSTRAP.is_ascii());
        assert!(WEB_API_BOOTSTRAP.contains("adoptInterface('Event'"));
        assert!(WEB_API_BOOTSTRAP.contains("adoptInterface('DOMMatrix'"));
        assert!(WEB_API_BOOTSTRAP.contains("adoptInterface('Headers'"));
        assert!(WEB_API_BOOTSTRAP.contains("op_vixen_storage_set"));
        assert!(WEB_API_BOOTSTRAP.contains("structuredClone"));
    }
}
