//! vixen-store — redb-backed persistence.
//!
//! Per-origin partitioned storage for cookies, fetch cache, history, sessions,
//! downloads, and Web Storage (docs/ARCHITECTURE.md "App ID and profile
//! paths"). The crate is deliberately independent of `vixen-net`: callers pass
//! an opaque `origin_key` (e.g. an `Origin::partition_key()`) so store never
//! depends on networking. Every table namespaces by that key so cross-origin
//! reads are impossible (docs/SPEC.md origin isolation).

#![forbid(unsafe_code)]

use std::path::Path;

use redb::{Database, ReadableTable, TableDefinition};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

// One table per concern (docs/ARCHITECTURE.md profile layout). Keys are
// `&[u8]` prefixed with the origin partition key; values are bounded JSON
// serde bytes.
const COOKIES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("cookies");
const FETCH_CACHE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("fetch-cache");
const FETCH_CACHE_ALIASES: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("fetch-cache-aliases");
const HISTORY: TableDefinition<&[u8], &[u8]> = TableDefinition::new("history");
const SESSION: TableDefinition<&[u8], &[u8]> = TableDefinition::new("session");
const WEB_STORAGE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("web-storage");
const DOWNLOADS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("downloads");
const PERMISSIONS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("permissions");
const HSTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("hsts");
const SESSION_KEY: &[u8] = b"open-tabs";
const SESSION_RECORD_KEY: &[u8] = b"session-record-v1";
pub const MAX_DOWNLOAD_RECORDS: usize = 512;
pub const MAX_FETCH_CACHE_RECORDS: usize = 512;
pub const MAX_FETCH_CACHE_ALIASES: usize = 512;
pub const MAX_FETCH_CACHE_REDIRECTS: usize = 20;
pub const MAX_SESSION_TABS: usize = 128;
pub const MAX_SESSION_FORM_CONTROLS: usize = 512;
const MAX_SESSION_URL_BYTES: usize = 8192;
const MAX_SESSION_FIELD_BYTES: usize = 8192;
const MAX_DOWNLOAD_FIELD_BYTES: usize = 8192;

/// Profile-wide data groups cleared by browser clear-data flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClearDataSelection {
    /// Partitioned HTTP cookies.
    pub cookies: bool,
    /// Partitioned GET response cache entries.
    pub fetch_cache: bool,
    /// Visited URL timestamps.
    pub history: bool,
    /// Saved open-tab session restore state.
    pub session: bool,
    /// `localStorage` / persisted `sessionStorage` partitions.
    pub web_storage: bool,
    /// Profile-wide download history.
    pub downloads: bool,
    /// Per-origin permission decisions.
    pub permissions: bool,
    /// HSTS and related persisted security state.
    pub security_state: bool,
}

impl ClearDataSelection {
    /// Clear every persisted profile table.
    pub const fn all() -> Self {
        Self {
            cookies: true,
            fetch_cache: true,
            history: true,
            session: true,
            web_storage: true,
            downloads: true,
            permissions: true,
            security_state: true,
        }
    }

    /// Clear user-visible browsing data while preserving session restore.
    pub const fn browsing_data() -> Self {
        Self {
            cookies: true,
            fetch_cache: true,
            history: true,
            session: false,
            web_storage: true,
            downloads: true,
            permissions: true,
            security_state: true,
        }
    }

    pub const fn is_empty(self) -> bool {
        !self.cookies
            && !self.fetch_cache
            && !self.history
            && !self.session
            && !self.web_storage
            && !self.downloads
            && !self.permissions
            && !self.security_state
    }
}

/// Persistent store backed by a single redb file.
pub struct Store {
    db: Database,
}

/// Errors are boxed because redb's error types are large (~160 B); this keeps
/// `Result<T, StoreError>` small on the stack.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(Box<redb::Error>),
    #[error("storage error: {0}")]
    Storage(Box<redb::StorageError>),
    #[error("transaction error: {0}")]
    Transaction(Box<redb::TransactionError>),
    #[error("table error: {0}")]
    Table(Box<redb::TableError>),
    #[error("commit error: {0}")]
    Commit(Box<redb::CommitError>),
    #[error("database open error: {0}")]
    DatabaseOpen(Box<redb::DatabaseError>),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("utf-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("invalid web storage {field}: {reason}")]
    InvalidWebStorageInput {
        field: &'static str,
        reason: &'static str,
    },
    #[error("invalid download {field}: {reason}")]
    InvalidDownloadInput {
        field: &'static str,
        reason: &'static str,
    },
    #[error("invalid session {field}: {reason}")]
    InvalidSessionInput {
        field: &'static str,
        reason: &'static str,
    },
    #[error("invalid permission {field}: {reason}")]
    InvalidPermissionInput {
        field: &'static str,
        reason: &'static str,
    },
    #[error("invalid security state {field}: {reason}")]
    InvalidSecurityStateInput {
        field: &'static str,
        reason: &'static str,
    },
    #[error("invalid fetch cache {field}: {reason}")]
    InvalidFetchCacheInput {
        field: &'static str,
        reason: &'static str,
    },
    #[error("table {0} not found")]
    MissingTable(&'static str),
}

impl From<redb::Error> for StoreError {
    fn from(e: redb::Error) -> Self {
        Self::Database(Box::new(e))
    }
}
impl From<redb::StorageError> for StoreError {
    fn from(e: redb::StorageError) -> Self {
        Self::Storage(Box::new(e))
    }
}
impl From<redb::TransactionError> for StoreError {
    fn from(e: redb::TransactionError) -> Self {
        Self::Transaction(Box::new(e))
    }
}
impl From<redb::TableError> for StoreError {
    fn from(e: redb::TableError) -> Self {
        Self::Table(Box::new(e))
    }
}
impl From<redb::CommitError> for StoreError {
    fn from(e: redb::CommitError) -> Self {
        Self::Commit(Box::new(e))
    }
}
impl From<redb::DatabaseError> for StoreError {
    fn from(e: redb::DatabaseError) -> Self {
        Self::DatabaseOpen(Box::new(e))
    }
}

type Result<T> = std::result::Result<T, StoreError>;

impl Store {
    /// Open (or create) the store at `path`. The profile directory must
    /// exist; the caller creates `~/.local/share/<app-id>/` first.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path)?;
        // Eagerly create every table so reads never see "missing table".
        let w = db.begin_write()?;
        {
            let _ = w.open_table(COOKIES)?;
            let _ = w.open_table(FETCH_CACHE)?;
            let _ = w.open_table(FETCH_CACHE_ALIASES)?;
            let _ = w.open_table(HISTORY)?;
            let _ = w.open_table(SESSION)?;
            let _ = w.open_table(WEB_STORAGE)?;
            let _ = w.open_table(DOWNLOADS)?;
            let _ = w.open_table(PERMISSIONS)?;
            let _ = w.open_table(HSTS)?;
        }
        w.commit()?;
        Ok(Self { db })
    }

    // --- Cookies ------------------------------------------------------------

    /// Insert/overwrite a cookie under `origin_key`.
    pub fn put_cookie(&self, origin_key: &str, rec: &CookieRecord) -> Result<()> {
        let key = namespaced_key(origin_key, &rec.name);
        let val = encode(rec)?;
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(COOKIES)?;
            t.insert(key.as_slice(), val.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    /// All cookies for `origin_key` (partitioned: never touches other origins).
    pub fn cookies_for(&self, origin_key: &str) -> Result<Vec<CookieRecord>> {
        let r = self.db.begin_read()?;
        let t = r
            .open_table(COOKIES)
            .map_err(|_| StoreError::MissingTable("cookies"))?;
        let prefix = namespaced_prefix(origin_key);
        let mut out = Vec::new();
        for item in t.iter()? {
            let (k, v) = item?;
            let k = k.value();
            if !k.starts_with(prefix.as_slice()) {
                continue;
            }
            if let Ok(rec) = decode::<CookieRecord>(v.value()) {
                out.push(rec);
            }
        }
        Ok(out)
    }

    /// Delete a single cookie by name under `origin_key`.
    pub fn delete_cookie(&self, origin_key: &str, name: &str) -> Result<()> {
        let key = namespaced_key(origin_key, name);
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(COOKIES)?;
            t.remove(key.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    /// Remove every cookie in one origin partition and no other partition.
    pub fn clear_cookies(&self, origin_key: &str) -> Result<()> {
        let prefix = namespaced_prefix(origin_key);
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(COOKIES)?;
            let mut keys = Vec::new();
            for item in t.iter()? {
                let (k, _) = item?;
                if k.value().starts_with(prefix.as_slice()) {
                    keys.push(k.value().to_vec());
                }
            }
            for key in keys {
                t.remove(key.as_slice())?;
            }
        }
        w.commit()?;
        Ok(())
    }

    // --- Fetch cache --------------------------------------------------------

    pub fn put_cache(&self, origin_key: &str, url: &str, entry: &CacheEntry) -> Result<()> {
        let key = fetch_cache_variant_key(origin_key, url, entry)?;
        let legacy_key = namespaced_key(origin_key, url);
        let val = encode(entry)?;
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(FETCH_CACHE)?;
            t.remove(legacy_key.as_slice())?;
            t.insert(key.as_slice(), val.as_slice())?;
            let mut records = Vec::new();
            for item in t.iter()? {
                let (k, v) = item?;
                let entry = decode::<CacheEntry>(v.value())?;
                records.push((k.value().to_vec(), entry.fetched_unix));
            }
            if records.len() > MAX_FETCH_CACHE_RECORDS {
                records.sort_by(|(key_a, fetched_a), (key_b, fetched_b)| {
                    fetched_a.cmp(fetched_b).then_with(|| key_a.cmp(key_b))
                });
                let remove_count = records.len() - MAX_FETCH_CACHE_RECORDS;
                for (key, _) in records.into_iter().take(remove_count) {
                    t.remove(key.as_slice())?;
                }
            }
        }
        w.commit()?;
        Ok(())
    }

    pub fn get_cache(&self, origin_key: &str, url: &str) -> Result<Option<CacheEntry>> {
        Ok(self
            .cache_variants(origin_key, url)?
            .into_iter()
            .max_by_key(|entry| entry.fetched_unix))
    }

    pub fn cache_variants(&self, origin_key: &str, url: &str) -> Result<Vec<CacheEntry>> {
        let r = self.db.begin_read()?;
        let t = r
            .open_table(FETCH_CACHE)
            .map_err(|_| StoreError::MissingTable("fetch-cache"))?;
        let legacy_key = namespaced_key(origin_key, url);
        let variant_prefix = fetch_cache_variant_prefix(origin_key, url);
        let mut entries = Vec::new();
        for item in t.iter()? {
            let (key, value) = item?;
            if key.value() == legacy_key.as_slice()
                || key.value().starts_with(variant_prefix.as_slice())
            {
                entries.push(decode(value.value())?);
            }
        }
        Ok(entries)
    }

    pub fn put_cache_alias(&self, origin_key: &str, url: &str, alias: &CacheAlias) -> Result<()> {
        validate_cache_alias(url, alias)?;
        let key = namespaced_key(origin_key, url);
        let value = encode(alias)?;
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(FETCH_CACHE_ALIASES)?;
            table.insert(key.as_slice(), value.as_slice())?;
            let mut records = Vec::new();
            for item in table.iter()? {
                let (key, value) = item?;
                let alias = decode::<CacheAlias>(value.value())?;
                records.push((key.value().to_vec(), alias.fetched_unix));
            }
            if records.len() > MAX_FETCH_CACHE_ALIASES {
                records.sort_by(|(key_a, fetched_a), (key_b, fetched_b)| {
                    fetched_a.cmp(fetched_b).then_with(|| key_a.cmp(key_b))
                });
                let remove_count = records.len() - MAX_FETCH_CACHE_ALIASES;
                for (key, _) in records.into_iter().take(remove_count) {
                    table.remove(key.as_slice())?;
                }
            }
        }
        write.commit()?;
        Ok(())
    }

    pub fn cache_alias(&self, origin_key: &str, url: &str) -> Result<Option<CacheAlias>> {
        let read = self.db.begin_read()?;
        let table = read
            .open_table(FETCH_CACHE_ALIASES)
            .map_err(|_| StoreError::MissingTable("fetch-cache-aliases"))?;
        let key = namespaced_key(origin_key, url);
        match table.get(key.as_slice())? {
            Some(value) => Ok(Some(decode(value.value())?)),
            None => Ok(None),
        }
    }

    pub fn delete_cache_alias(&self, origin_key: &str, url: &str) -> Result<()> {
        let key = namespaced_key(origin_key, url);
        let write = self.db.begin_write()?;
        {
            write
                .open_table(FETCH_CACHE_ALIASES)?
                .remove(key.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    // --- History ------------------------------------------------------------

    /// Record a visit to `url` at Unix second `ts`. Multiple visits append.
    pub fn record_visit(&self, origin_key: &str, url: &str, ts: i64) -> Result<()> {
        let key = namespaced_key(origin_key, url);
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(HISTORY)?;
            let visits = match t.get(key.as_slice())? {
                Some(v) => {
                    let mut v: Vec<i64> = decode(v.value())?;
                    v.push(ts);
                    v
                }
                None => vec![ts],
            };
            let val = encode(&visits)?;
            t.insert(key.as_slice(), val.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    pub fn visits_for(&self, origin_key: &str, url: &str) -> Result<Vec<i64>> {
        let r = self.db.begin_read()?;
        let t = r
            .open_table(HISTORY)
            .map_err(|_| StoreError::MissingTable("history"))?;
        let key = namespaced_key(origin_key, url);
        match t.get(key.as_slice())? {
            Some(v) => Ok(decode(v.value())?),
            None => Ok(Vec::new()),
        }
    }

    // --- Session ------------------------------------------------------------

    /// Persist the list of open-tab URLs (session restore).
    pub fn save_session(&self, tabs: &[String]) -> Result<()> {
        let record = SessionRecord {
            tabs: tabs.to_vec(),
            active_index: 0,
            tab_states: Vec::new(),
        };
        self.save_session_record(&record)
    }

    /// Persist deterministic session-restore metadata.
    pub fn save_session_record(&self, record: &SessionRecord) -> Result<()> {
        validate_session_record(record)?;
        let tabs = &record.tabs;
        let val = encode(tabs)?;
        let record_val = encode(record)?;
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(SESSION)?;
            // Keep the legacy open-tabs value in sync so older callers remain
            // compatible while newer shell/session code consumes active_index.
            t.insert(SESSION_KEY, val.as_slice())?;
            t.insert(SESSION_RECORD_KEY, record_val.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    pub fn load_session(&self) -> Result<Vec<String>> {
        self.load_session_record().map(|record| record.tabs)
    }

    /// Load deterministic session-restore metadata, falling back to the legacy
    /// open-tabs list for profiles written before active-tab tracking existed.
    pub fn load_session_record(&self) -> Result<SessionRecord> {
        let r = self.db.begin_read()?;
        let t = r
            .open_table(SESSION)
            .map_err(|_| StoreError::MissingTable("session"))?;
        if let Some(v) = t.get(SESSION_RECORD_KEY)? {
            let record = decode::<SessionRecord>(v.value())?;
            validate_session_record(&record)?;
            return Ok(record);
        }
        match t.get(SESSION_KEY)? {
            Some(v) => {
                let tabs = decode::<Vec<String>>(v.value())?;
                let record = SessionRecord {
                    tabs,
                    active_index: 0,
                    tab_states: Vec::new(),
                };
                validate_session_record(&record)?;
                Ok(record)
            }
            None => Ok(SessionRecord::default()),
        }
    }

    // --- Downloads ----------------------------------------------------------

    /// Insert or update one profile-wide download history record.
    ///
    /// The table is bounded so download-heavy sessions cannot grow the profile
    /// without limit before the shell's clear-data UI exists.
    pub fn put_download(&self, record: &DownloadRecord) -> Result<()> {
        validate_download_record(record)?;
        let key = record.id.to_be_bytes();
        let val = encode(record)?;
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(DOWNLOADS)?;
            t.insert(key.as_slice(), val.as_slice())?;
            let mut records = Vec::new();
            for item in t.iter()? {
                let (k, v) = item?;
                let rec = decode::<DownloadRecord>(v.value())?;
                records.push((k.value().to_vec(), rec.started_unix, rec.id));
            }
            if records.len() > MAX_DOWNLOAD_RECORDS {
                records.sort_by_key(|(_, started_unix, id)| (*started_unix, *id));
                let remove_count = records.len() - MAX_DOWNLOAD_RECORDS;
                for (key, _, _) in records.into_iter().take(remove_count) {
                    t.remove(key.as_slice())?;
                }
            }
        }
        w.commit()?;
        Ok(())
    }

    /// Return profile-wide downloads newest-first.
    pub fn downloads(&self) -> Result<Vec<DownloadRecord>> {
        let r = self.db.begin_read()?;
        let t = r
            .open_table(DOWNLOADS)
            .map_err(|_| StoreError::MissingTable("downloads"))?;
        let mut out = Vec::new();
        for item in t.iter()? {
            let (_, v) = item?;
            out.push(decode::<DownloadRecord>(v.value())?);
        }
        out.sort_by(|a, b| {
            b.started_unix
                .cmp(&a.started_unix)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(out)
    }

    /// Remove every persisted download history record.
    pub fn clear_downloads(&self) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(DOWNLOADS)?;
            let mut keys = Vec::new();
            for item in t.iter()? {
                let (k, _) = item?;
                keys.push(k.value().to_vec());
            }
            for key in keys {
                t.remove(key.as_slice())?;
            }
        }
        w.commit()?;
        Ok(())
    }

    /// Remove selected persisted profile data in one clear-data transaction.
    pub fn clear_profile_data(&self, selection: ClearDataSelection) -> Result<()> {
        if selection.is_empty() {
            return Ok(());
        }

        let w = self.db.begin_write()?;
        if selection.cookies {
            clear_table(&w, COOKIES)?;
        }
        if selection.fetch_cache {
            clear_table(&w, FETCH_CACHE)?;
            clear_table(&w, FETCH_CACHE_ALIASES)?;
        }
        if selection.history {
            clear_table(&w, HISTORY)?;
        }
        if selection.session {
            clear_table(&w, SESSION)?;
        }
        if selection.web_storage {
            clear_table(&w, WEB_STORAGE)?;
        }
        if selection.downloads {
            clear_table(&w, DOWNLOADS)?;
        }
        if selection.permissions {
            clear_table(&w, PERMISSIONS)?;
        }
        if selection.security_state {
            clear_table(&w, HSTS)?;
        }
        w.commit()?;
        Ok(())
    }

    // --- Security state -----------------------------------------------------

    /// Insert or update one HTTP Strict Transport Security entry keyed by host.
    pub fn put_hsts(&self, record: &HstsRecord) -> Result<()> {
        validate_hsts_record(record)?;
        let key = hsts_key(&record.host);
        let val = encode(record)?;
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(HSTS)?;
            t.insert(key.as_slice(), val.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    /// Read one HSTS entry by host.
    pub fn hsts(&self, host: &str) -> Result<Option<HstsRecord>> {
        validate_hsts_host(host)?;
        let r = self.db.begin_read()?;
        let t = r
            .open_table(HSTS)
            .map_err(|_| StoreError::MissingTable("hsts"))?;
        let key = hsts_key(host);
        match t.get(key.as_slice())? {
            Some(v) => Ok(Some(decode(v.value())?)),
            None => Ok(None),
        }
    }

    /// Remove an HSTS entry, e.g. after receiving `max-age=0`.
    pub fn delete_hsts(&self, host: &str) -> Result<()> {
        validate_hsts_host(host)?;
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(HSTS)?;
            let key = hsts_key(host);
            t.remove(key.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    // --- Permissions --------------------------------------------------------

    /// Insert or update one per-origin permission decision.
    pub fn put_permission(&self, record: &PermissionRecord) -> Result<()> {
        validate_permission_record(record)?;
        let key = namespaced_key(&record.origin_key, &record.kind);
        let val = encode(record)?;
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(PERMISSIONS)?;
            t.insert(key.as_slice(), val.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    /// Read one permission decision. Unknown permissions return `None` so the
    /// caller can fail closed to its default prompt/deny behavior.
    pub fn permission(&self, origin_key: &str, kind: &str) -> Result<Option<PermissionRecord>> {
        validate_permission_text("origin_key", origin_key)?;
        validate_permission_text("kind", kind)?;
        let r = self.db.begin_read()?;
        let t = r
            .open_table(PERMISSIONS)
            .map_err(|_| StoreError::MissingTable("permissions"))?;
        let key = namespaced_key(origin_key, kind);
        match t.get(key.as_slice())? {
            Some(v) => Ok(Some(decode(v.value())?)),
            None => Ok(None),
        }
    }

    /// Return every permission decision for one origin partition.
    pub fn permissions_for(&self, origin_key: &str) -> Result<Vec<PermissionRecord>> {
        validate_permission_text("origin_key", origin_key)?;
        let prefix = namespaced_prefix(origin_key);
        let r = self.db.begin_read()?;
        let t = r
            .open_table(PERMISSIONS)
            .map_err(|_| StoreError::MissingTable("permissions"))?;
        let mut out = Vec::new();
        for item in t.iter()? {
            let (k, v) = item?;
            if k.value().starts_with(prefix.as_slice()) {
                out.push(decode::<PermissionRecord>(v.value())?);
            }
        }
        out.sort_by(|a, b| a.kind.cmp(&b.kind));
        Ok(out)
    }

    /// Remove every permission decision for one origin partition.
    pub fn clear_permissions(&self, origin_key: &str) -> Result<()> {
        validate_permission_text("origin_key", origin_key)?;
        let prefix = namespaced_prefix(origin_key);
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(PERMISSIONS)?;
            let mut keys = Vec::new();
            for item in t.iter()? {
                let (k, _) = item?;
                if k.value().starts_with(prefix.as_slice()) {
                    keys.push(k.value().to_vec());
                }
            }
            for key in keys {
                t.remove(key.as_slice())?;
            }
        }
        w.commit()?;
        Ok(())
    }

    // --- Web Storage --------------------------------------------------------

    /// Insert/overwrite a Web Storage item under a caller-derived partition key.
    ///
    /// `partition_key` should be the `storage:{kind}:{origin}` string produced
    /// by the engine's storage partition logic. This crate deliberately keeps it
    /// opaque so it remains independent of `vixen-engine` and `vixen-net`.
    pub fn put_storage_item(&self, partition_key: &str, key: &str, value: &str) -> Result<()> {
        validate_storage_input("partition", partition_key, false)?;
        validate_storage_input("key", key, false)?;
        validate_storage_input("value", value, true)?;

        let db_key = namespaced_key(partition_key, key);
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(WEB_STORAGE)?;
            let sequence = match t.get(db_key.as_slice())? {
                Some(existing) => decode::<WebStorageRecord>(existing.value())?.sequence,
                None => next_web_storage_sequence(&t, partition_key)?,
            };
            let rec = WebStorageRecord {
                value: value.to_owned(),
                sequence,
            };
            let val = encode(&rec)?;
            t.insert(db_key.as_slice(), val.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    /// Read one Web Storage item from a partition.
    pub fn get_storage_item(&self, partition_key: &str, key: &str) -> Result<Option<String>> {
        validate_storage_input("partition", partition_key, false)?;
        validate_storage_input("key", key, false)?;
        let r = self.db.begin_read()?;
        let t = r
            .open_table(WEB_STORAGE)
            .map_err(|_| StoreError::MissingTable("web-storage"))?;
        let db_key = namespaced_key(partition_key, key);
        match t.get(db_key.as_slice())? {
            Some(v) => Ok(Some(decode::<WebStorageRecord>(v.value())?.value)),
            None => Ok(None),
        }
    }

    /// Remove one Web Storage item from a partition.
    pub fn remove_storage_item(&self, partition_key: &str, key: &str) -> Result<()> {
        validate_storage_input("partition", partition_key, false)?;
        validate_storage_input("key", key, false)?;
        let db_key = namespaced_key(partition_key, key);
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(WEB_STORAGE)?;
            t.remove(db_key.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    /// Remove every Web Storage item for one partition and no other partition.
    pub fn clear_storage_partition(&self, partition_key: &str) -> Result<()> {
        validate_storage_input("partition", partition_key, false)?;
        let prefix = namespaced_prefix(partition_key);
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(WEB_STORAGE)?;
            let mut keys = Vec::new();
            for item in t.iter()? {
                let (k, _) = item?;
                if k.value().starts_with(prefix.as_slice()) {
                    keys.push(k.value().to_vec());
                }
            }
            for key in keys {
                t.remove(key.as_slice())?;
            }
        }
        w.commit()?;
        Ok(())
    }

    /// Return Web Storage entries in stable insertion order for host `key(n)` /
    /// enumeration projections.
    pub fn storage_entries(&self, partition_key: &str) -> Result<Vec<(String, String)>> {
        validate_storage_input("partition", partition_key, false)?;
        let prefix = namespaced_prefix(partition_key);
        let r = self.db.begin_read()?;
        let t = r
            .open_table(WEB_STORAGE)
            .map_err(|_| StoreError::MissingTable("web-storage"))?;
        let mut out = Vec::new();
        for item in t.iter()? {
            let (k, v) = item?;
            let Some(raw_key) = k.value().strip_prefix(prefix.as_slice()) else {
                continue;
            };
            let rec = decode::<WebStorageRecord>(v.value())?;
            out.push((
                rec.sequence,
                String::from_utf8(raw_key.to_vec())?,
                rec.value,
            ));
        }
        out.sort_by_key(|(sequence, _, _)| *sequence);
        Ok(out
            .into_iter()
            .map(|(_, key, value)| (key, value))
            .collect())
    }
}

fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(StoreError::from)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(StoreError::from)
}

fn clear_table(
    tx: &redb::WriteTransaction,
    table: TableDefinition<&'static [u8], &'static [u8]>,
) -> Result<()> {
    let mut table = tx.open_table(table)?;
    let mut keys = Vec::new();
    for item in table.iter()? {
        let (key, _) = item?;
        keys.push(key.value().to_vec());
    }
    for key in keys {
        table.remove(key.as_slice())?;
    }
    Ok(())
}

fn hsts_key(host: &str) -> Vec<u8> {
    host.to_ascii_lowercase().into_bytes()
}

/// Build `<origin_key> \x00 <name>` so origin partitions never collide.
fn namespaced_key(origin_key: &str, name: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(origin_key.len() + 1 + name.len());
    k.extend_from_slice(origin_key.as_bytes());
    k.push(0);
    k.extend_from_slice(name.as_bytes());
    k
}

fn namespaced_prefix(origin_key: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(origin_key.len() + 1);
    k.extend_from_slice(origin_key.as_bytes());
    k.push(0);
    k
}

const FETCH_CACHE_VARIANT_MARKER: &[u8] = b"\0v1\0";

fn fetch_cache_variant_prefix(origin_key: &str, url: &str) -> Vec<u8> {
    let mut key = namespaced_key(origin_key, url);
    key.extend_from_slice(FETCH_CACHE_VARIANT_MARKER);
    key
}

fn fetch_cache_variant_key(origin_key: &str, url: &str, entry: &CacheEntry) -> Result<Vec<u8>> {
    let mut selector = entry.vary_headers.clone();
    selector.sort();
    let selector = encode(&selector)?;
    let mut key = fetch_cache_variant_prefix(origin_key, url);
    key.extend_from_slice(&selector);
    Ok(key)
}

fn validate_storage_input(field: &'static str, value: &str, allow_empty: bool) -> Result<()> {
    if !allow_empty && value.is_empty() {
        return Err(StoreError::InvalidWebStorageInput {
            field,
            reason: "must be non-empty",
        });
    }
    if value.as_bytes().contains(&0) {
        return Err(StoreError::InvalidWebStorageInput {
            field,
            reason: "must not contain NUL bytes",
        });
    }
    Ok(())
}

const MAX_FETCH_CACHE_ALIAS_BYTES: usize = 64 * 1024;

fn validate_cache_alias(url: &str, alias: &CacheAlias) -> Result<()> {
    if url.is_empty() || url.as_bytes().contains(&0) {
        return Err(StoreError::InvalidFetchCacheInput {
            field: "alias-url",
            reason: "must be non-empty and contain no NUL bytes",
        });
    }
    if alias.hops.is_empty() || alias.hops.len() > MAX_FETCH_CACHE_REDIRECTS {
        return Err(StoreError::InvalidFetchCacheInput {
            field: "alias-hops",
            reason: "must contain between one and twenty redirects",
        });
    }
    let mut total_bytes = 0_usize;
    for hop in &alias.hops {
        if !matches!(hop.status, 301 | 308) {
            return Err(StoreError::InvalidFetchCacheInput {
                field: "alias-status",
                reason: "must be a permanent redirect",
            });
        }
        if hop.to.is_empty() || hop.to.as_bytes().contains(&0) {
            return Err(StoreError::InvalidFetchCacheInput {
                field: "alias-target",
                reason: "must be non-empty and contain no NUL bytes",
            });
        }
        total_bytes =
            total_bytes
                .checked_add(hop.to.len())
                .ok_or(StoreError::InvalidFetchCacheInput {
                    field: "alias-target",
                    reason: "total target bytes overflow",
                })?;
        if total_bytes > MAX_FETCH_CACHE_ALIAS_BYTES {
            return Err(StoreError::InvalidFetchCacheInput {
                field: "alias-target",
                reason: "total target bytes exceed 64 KiB",
            });
        }
    }
    Ok(())
}

fn next_web_storage_sequence(
    table: &impl ReadableTable<&'static [u8], &'static [u8]>,
    partition_key: &str,
) -> Result<u64> {
    let prefix = namespaced_prefix(partition_key);
    let mut max_sequence = 0;
    for item in table.iter()? {
        let (k, v) = item?;
        if !k.value().starts_with(prefix.as_slice()) {
            continue;
        }
        let rec = decode::<WebStorageRecord>(v.value())?;
        max_sequence = max_sequence.max(rec.sequence);
    }
    Ok(max_sequence + 1)
}

fn validate_download_record(record: &DownloadRecord) -> Result<()> {
    validate_download_text("filename", &record.filename, false)?;
    validate_download_text("mime", &record.mime, true)?;
    if let Some(source_url) = &record.source_url {
        validate_download_text("source_url", source_url, false)?;
    }
    if let Some(destination_path) = &record.destination_path {
        validate_download_text("destination_path", destination_path, false)?;
    }
    if let Some(error) = &record.error {
        validate_download_text("error", error, false)?;
    }
    if let Some(total_bytes) = record.total_bytes
        && record.received_bytes > total_bytes
    {
        return Err(StoreError::InvalidDownloadInput {
            field: "received_bytes",
            reason: "must not exceed total_bytes",
        });
    }
    if record.updated_unix < record.started_unix {
        return Err(StoreError::InvalidDownloadInput {
            field: "updated_unix",
            reason: "must be greater than or equal to started_unix",
        });
    }
    Ok(())
}

fn validate_session_record(record: &SessionRecord) -> Result<()> {
    if record.tabs.len() > MAX_SESSION_TABS {
        return Err(StoreError::InvalidSessionInput {
            field: "tabs",
            reason: "exceeds maximum tab count",
        });
    }
    if record.tabs.is_empty() {
        if record.active_index != 0 {
            return Err(StoreError::InvalidSessionInput {
                field: "active_index",
                reason: "must be zero for an empty session",
            });
        }
        if !record.tab_states.is_empty() {
            return Err(StoreError::InvalidSessionInput {
                field: "tab_states",
                reason: "must be empty for an empty session",
            });
        }
        return Ok(());
    }
    if record.active_index >= record.tabs.len() {
        return Err(StoreError::InvalidSessionInput {
            field: "active_index",
            reason: "must reference an existing tab",
        });
    }
    if !record.tab_states.is_empty() && record.tab_states.len() != record.tabs.len() {
        return Err(StoreError::InvalidSessionInput {
            field: "tab_states",
            reason: "must be empty or match the tab count",
        });
    }
    for tab in &record.tabs {
        validate_session_url(tab)?;
    }
    for state in &record.tab_states {
        validate_tab_session_state(state)?;
    }
    Ok(())
}

fn validate_tab_session_state(state: &TabSessionState) -> Result<()> {
    if let Some(focused_element) = &state.focused_element {
        validate_session_field("focused_element", focused_element, false)?;
    }
    if state.form_controls.len() > MAX_SESSION_FORM_CONTROLS {
        return Err(StoreError::InvalidSessionInput {
            field: "form_controls",
            reason: "exceeds maximum form control count",
        });
    }
    for control in &state.form_controls {
        validate_session_field("form_controls", &control.key, false)?;
        validate_session_field("form_controls", &control.value, true)?;
    }
    Ok(())
}

fn validate_session_url(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(StoreError::InvalidSessionInput {
            field: "tabs",
            reason: "must contain non-empty URLs",
        });
    }
    if value.len() > MAX_SESSION_URL_BYTES {
        return Err(StoreError::InvalidSessionInput {
            field: "tabs",
            reason: "URL exceeds maximum length",
        });
    }
    if value.as_bytes().contains(&0) {
        return Err(StoreError::InvalidSessionInput {
            field: "tabs",
            reason: "URL must not contain NUL bytes",
        });
    }
    Ok(())
}

fn validate_session_field(field: &'static str, value: &str, allow_empty: bool) -> Result<()> {
    if !allow_empty && value.is_empty() {
        return Err(StoreError::InvalidSessionInput {
            field,
            reason: "must be non-empty",
        });
    }
    if value.len() > MAX_SESSION_FIELD_BYTES {
        return Err(StoreError::InvalidSessionInput {
            field,
            reason: "exceeds maximum length",
        });
    }
    if value.as_bytes().contains(&0) {
        return Err(StoreError::InvalidSessionInput {
            field,
            reason: "must not contain NUL bytes",
        });
    }
    Ok(())
}

fn validate_permission_record(record: &PermissionRecord) -> Result<()> {
    validate_permission_text("origin_key", &record.origin_key)?;
    validate_permission_text("kind", &record.kind)?;
    Ok(())
}

fn validate_permission_text(field: &'static str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(StoreError::InvalidPermissionInput {
            field,
            reason: "must be non-empty",
        });
    }
    if value.as_bytes().contains(&0) {
        return Err(StoreError::InvalidPermissionInput {
            field,
            reason: "must not contain NUL bytes",
        });
    }
    Ok(())
}

fn validate_hsts_record(record: &HstsRecord) -> Result<()> {
    validate_hsts_host(&record.host)?;
    if record.expires_unix < record.received_unix {
        return Err(StoreError::InvalidSecurityStateInput {
            field: "expires_unix",
            reason: "must be greater than or equal to received_unix",
        });
    }
    Ok(())
}

fn validate_hsts_host(host: &str) -> Result<()> {
    if host.is_empty() {
        return Err(StoreError::InvalidSecurityStateInput {
            field: "host",
            reason: "must be non-empty",
        });
    }
    if host.as_bytes().contains(&0) {
        return Err(StoreError::InvalidSecurityStateInput {
            field: "host",
            reason: "must not contain NUL bytes",
        });
    }
    Ok(())
}

fn validate_download_text(field: &'static str, value: &str, allow_empty: bool) -> Result<()> {
    if !allow_empty && value.is_empty() {
        return Err(StoreError::InvalidDownloadInput {
            field,
            reason: "must be non-empty",
        });
    }
    if value.len() > MAX_DOWNLOAD_FIELD_BYTES {
        return Err(StoreError::InvalidDownloadInput {
            field,
            reason: "exceeds maximum length",
        });
    }
    if value.as_bytes().contains(&0) {
        return Err(StoreError::InvalidDownloadInput {
            field,
            reason: "must not contain NUL bytes",
        });
    }
    Ok(())
}

// --- Record types -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CookieRecord {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    pub expires_unix: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    /// 0 = Strict, 1 = Lax, 2 = None (matches docs/SPEC.md "Cookie defaults").
    pub same_site: u8,
    pub creation_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheEntry {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub fetched_unix: i64,
    /// Request-header values selected by the response's `Vary` field. Empty
    /// for responses without `Vary` and legacy profile entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vary_headers: Vec<(String, Option<String>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheAlias {
    pub hops: Vec<CacheRedirectHop>,
    pub fetched_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheRedirectHop {
    pub to: String,
    pub status: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub tabs: Vec<String>,
    pub active_index: usize,
    /// Optional per-tab restore hints aligned with `tabs` when present.
    ///
    /// Legacy callers may leave this empty; callers that persist state should
    /// write one entry per tab so restore can reapply scroll, focus, and form
    /// control values without guessing across tabs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tab_states: Vec<TabSessionState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabSessionState {
    pub scroll_x: u32,
    pub scroll_y: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_element: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub form_controls: Vec<FormControlSessionState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormControlSessionState {
    /// Caller-owned stable control key, such as a DOM path/name fingerprint.
    pub key: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PermissionDecision {
    Granted,
    Denied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionRecord {
    pub origin_key: String,
    pub kind: String,
    pub decision: PermissionDecision,
    pub updated_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HstsRecord {
    pub host: String,
    pub include_subdomains: bool,
    pub expires_unix: i64,
    pub received_unix: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DownloadState {
    InProgress,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadRecord {
    pub id: u64,
    pub source_url: Option<String>,
    pub filename: String,
    pub destination_path: Option<String>,
    pub mime: String,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub state: DownloadState,
    pub started_unix: i64,
    pub updated_unix: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WebStorageRecord {
    value: String,
    sequence: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_store() -> (tempfile::NamedTempFile, Store) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(f.path()).unwrap();
        (f, store)
    }

    fn cookie(name: &str, value: &str) -> CookieRecord {
        CookieRecord {
            name: name.into(),
            value: value.into(),
            domain: "example.com".into(),
            host_only: false,
            path: "/".into(),
            expires_unix: Some(2_000_000_000),
            secure: true,
            http_only: false,
            same_site: 1,
            creation_unix: 1_000,
        }
    }

    fn download(id: u64, started_unix: i64) -> DownloadRecord {
        DownloadRecord {
            id,
            source_url: Some(format!("https://example.test/file-{id}.bin")),
            filename: format!("file-{id}.bin"),
            destination_path: Some(format!("/home/user/Downloads/file-{id}.bin")),
            mime: "application/octet-stream".to_owned(),
            received_bytes: id * 10,
            total_bytes: Some(10_000),
            state: DownloadState::InProgress,
            started_unix,
            updated_unix: started_unix,
            error: None,
        }
    }

    fn cache_entry(fetched_unix: i64) -> CacheEntry {
        CacheEntry {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: format!("<html>{fetched_unix}</html>").into_bytes(),
            fetched_unix,
            vary_headers: Vec::new(),
        }
    }

    fn permission(origin_key: &str, kind: &str, decision: PermissionDecision) -> PermissionRecord {
        PermissionRecord {
            origin_key: origin_key.to_owned(),
            kind: kind.to_owned(),
            decision,
            updated_unix: 1_234,
        }
    }

    fn hsts(host: &str) -> HstsRecord {
        HstsRecord {
            host: host.to_owned(),
            include_subdomains: true,
            expires_unix: 2_000,
            received_unix: 1_000,
        }
    }

    fn populate_profile_data(store: &Store) {
        store
            .put_cookie("https://clear.test:443", &cookie("sid", "clear"))
            .unwrap();
        store
            .put_cache(
                "https://clear.test:443",
                "https://clear.test/page",
                &cache_entry(123),
            )
            .unwrap();
        store
            .put_cache_alias(
                "https://clear.test:443",
                "https://clear.test/old",
                &CacheAlias {
                    hops: vec![CacheRedirectHop {
                        to: "https://clear.test/page".to_owned(),
                        status: 301,
                    }],
                    fetched_unix: 123,
                },
            )
            .unwrap();
        store
            .record_visit("https://clear.test:443", "https://clear.test/page", 456)
            .unwrap();
        store
            .save_session(&["https://clear.test/page".to_owned()])
            .unwrap();
        store
            .put_storage_item("storage:local:https://clear.test:443", "theme", "dark")
            .unwrap();
        store.put_download(&download(99, 789)).unwrap();
        store
            .put_permission(&permission(
                "https://clear.test:443",
                "notifications",
                PermissionDecision::Denied,
            ))
            .unwrap();
        store.put_hsts(&hsts("clear.test")).unwrap();
    }

    #[test]
    fn cookie_round_trips() {
        let (_f, store) = fresh_store();
        store
            .put_cookie("https://example.com:443", &cookie("sid", "abc"))
            .unwrap();
        let got = store.cookies_for("https://example.com:443").unwrap();
        assert_eq!(got, vec![cookie("sid", "abc")]);
    }

    #[test]
    fn cookies_are_origin_partitioned() {
        let (_f, store) = fresh_store();
        store
            .put_cookie("https://a.test:443", &cookie("sid", "from-a"))
            .unwrap();
        store
            .put_cookie("https://b.test:443", &cookie("sid", "from-b"))
            .unwrap();
        // Each origin sees only its own cookies — never the other's.
        assert_eq!(
            store.cookies_for("https://a.test:443").unwrap()[0].value,
            "from-a"
        );
        assert_eq!(
            store.cookies_for("https://b.test:443").unwrap()[0].value,
            "from-b"
        );
        // An unrelated origin sees nothing.
        assert!(store.cookies_for("https://c.test:443").unwrap().is_empty());
    }

    #[test]
    fn cookie_delete() {
        let (_f, store) = fresh_store();
        store
            .put_cookie("https://a.test:443", &cookie("sid", "x"))
            .unwrap();
        store.delete_cookie("https://a.test:443", "sid").unwrap();
        assert!(store.cookies_for("https://a.test:443").unwrap().is_empty());
    }

    #[test]
    fn clear_cookies_is_origin_partitioned() {
        let (_f, store) = fresh_store();
        store
            .put_cookie("https://a.test:443", &cookie("sid", "a"))
            .unwrap();
        store
            .put_cookie("https://b.test:443", &cookie("sid", "b"))
            .unwrap();

        store.clear_cookies("https://a.test:443").unwrap();

        assert!(store.cookies_for("https://a.test:443").unwrap().is_empty());
        assert_eq!(
            store.cookies_for("https://b.test:443").unwrap()[0].value,
            "b"
        );
    }

    #[test]
    fn fetch_cache_round_trips() {
        let (_f, store) = fresh_store();
        let entry = cache_entry(1_234);
        store
            .put_cache("https://a.test:443", "https://a.test/page", &entry)
            .unwrap();
        let got = store
            .get_cache("https://a.test:443", "https://a.test/page")
            .unwrap()
            .unwrap();
        assert_eq!(got, entry);
        // Partitioned: another origin can't read it.
        assert!(
            store
                .get_cache("https://b.test:443", "https://a.test/page")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn fetch_cache_retains_simultaneous_vary_variants() {
        let (_f, store) = fresh_store();
        let origin = "https://variants.test:443";
        let url = "https://variants.test/data";
        let mut english = cache_entry(100);
        english
            .headers
            .push(("vary".to_owned(), "Accept-Language, X-Mode".to_owned()));
        english.vary_headers = vec![
            ("accept-language".to_owned(), Some("en".to_owned())),
            ("x-mode".to_owned(), None),
        ];
        let mut french = english.clone();
        french.fetched_unix = 101;
        french.vary_headers[0].1 = Some("fr".to_owned());

        store.put_cache(origin, url, &english).unwrap();
        store.put_cache(origin, url, &french).unwrap();
        let mut refreshed_english = english.clone();
        refreshed_english.fetched_unix = 102;
        refreshed_english.vary_headers.reverse();
        store.put_cache(origin, url, &refreshed_english).unwrap();

        let mut variants = store.cache_variants(origin, url).unwrap();
        variants.sort_by_key(|entry| entry.fetched_unix);
        assert_eq!(variants, vec![french, refreshed_english.clone()]);
        assert_eq!(
            store.get_cache(origin, url).unwrap(),
            Some(refreshed_english)
        );
    }

    #[test]
    fn fetch_cache_reads_and_replaces_legacy_url_key() {
        let (_f, store) = fresh_store();
        let origin = "https://legacy-cache.test:443";
        let url = "https://legacy-cache.test/data";
        let legacy = cache_entry(100);
        let key = namespaced_key(origin, url);
        let value = encode(&legacy).unwrap();
        let write = store.db.begin_write().unwrap();
        {
            let mut table = write.open_table(FETCH_CACHE).unwrap();
            table.insert(key.as_slice(), value.as_slice()).unwrap();
        }
        write.commit().unwrap();

        assert_eq!(store.cache_variants(origin, url).unwrap(), vec![legacy]);

        let current = cache_entry(101);
        store.put_cache(origin, url, &current).unwrap();
        assert_eq!(store.cache_variants(origin, url).unwrap(), vec![current]);
    }

    #[test]
    fn fetch_cache_aliases_are_bounded_validated_and_deletable() {
        let (file, store) = fresh_store();
        let origin = "https://alias.test:443";
        let url = "https://alias.test/old";
        let alias = CacheAlias {
            hops: vec![
                CacheRedirectHop {
                    to: "https://alias.test/moved".to_owned(),
                    status: 301,
                },
                CacheRedirectHop {
                    to: "https://alias.test/final".to_owned(),
                    status: 308,
                },
            ],
            fetched_unix: 100,
        };

        store.put_cache_alias(origin, url, &alias).unwrap();
        drop(store);
        let store = Store::open(file.path()).unwrap();
        assert_eq!(store.cache_alias(origin, url).unwrap(), Some(alias));
        assert!(
            store
                .cache_alias("https://other.test:443", url)
                .unwrap()
                .is_none()
        );

        let invalid = CacheAlias {
            hops: vec![CacheRedirectHop {
                to: "https://alias.test/temporary".to_owned(),
                status: 302,
            }],
            fetched_unix: 101,
        };
        assert!(matches!(
            store.put_cache_alias(origin, url, &invalid),
            Err(StoreError::InvalidFetchCacheInput {
                field: "alias-status",
                ..
            })
        ));

        store.delete_cache_alias(origin, url).unwrap();
        assert!(store.cache_alias(origin, url).unwrap().is_none());

        for id in 0..(MAX_FETCH_CACHE_ALIASES + 2) {
            store
                .put_cache_alias(
                    origin,
                    &format!("https://alias.test/old-{id}"),
                    &CacheAlias {
                        hops: vec![CacheRedirectHop {
                            to: format!("https://alias.test/final-{id}"),
                            status: 301,
                        }],
                        fetched_unix: id as i64,
                    },
                )
                .unwrap();
        }
        assert!(
            store
                .cache_alias(origin, "https://alias.test/old-0")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .cache_alias(origin, "https://alias.test/old-2")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn legacy_fetch_cache_entry_defaults_vary_metadata() {
        let entry: CacheEntry = serde_json::from_str(
            r#"{"status":200,"headers":[],"body":[111,107],"fetched_unix":123}"#,
        )
        .unwrap();

        assert_eq!(entry.body, b"ok");
        assert!(entry.vary_headers.is_empty());
    }

    #[test]
    fn fetch_cache_is_bounded_to_newest_records() {
        let (_f, store) = fresh_store();
        let extra = 3;
        for id in 0..(MAX_FETCH_CACHE_RECORDS as u64 + extra) {
            store
                .put_cache(
                    "https://cache.test:443",
                    &format!("https://cache.test/item-{id}"),
                    &cache_entry(id as i64),
                )
                .unwrap();
        }

        assert!(
            store
                .get_cache("https://cache.test:443", "https://cache.test/item-0")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_cache(
                    "https://cache.test:443",
                    &format!("https://cache.test/item-{extra}")
                )
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_cache(
                    "https://cache.test:443",
                    &format!(
                        "https://cache.test/item-{}",
                        MAX_FETCH_CACHE_RECORDS as u64 + extra - 1
                    )
                )
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn web_storage_item_round_trips_in_insertion_order() {
        let (_f, store) = fresh_store();
        let partition = "storage:local:https://a.test:443";

        store.put_storage_item(partition, "theme", "dark").unwrap();
        store.put_storage_item(partition, "mode", "reader").unwrap();
        store.put_storage_item(partition, "theme", "light").unwrap();

        assert_eq!(
            store.get_storage_item(partition, "theme").unwrap(),
            Some("light".to_owned())
        );
        assert_eq!(
            store.storage_entries(partition).unwrap(),
            vec![
                ("theme".to_owned(), "light".to_owned()),
                ("mode".to_owned(), "reader".to_owned()),
            ]
        );
    }

    #[test]
    fn web_storage_partitions_by_origin_and_kind() {
        let (_f, store) = fresh_store();
        let local_a = "storage:local:https://a.test:443";
        let session_a = "storage:session:https://a.test:443";
        let local_b = "storage:local:https://b.test:443";

        store.put_storage_item(local_a, "token", "a-local").unwrap();
        store
            .put_storage_item(session_a, "token", "a-session")
            .unwrap();
        store.put_storage_item(local_b, "token", "b-local").unwrap();

        assert_eq!(
            store.get_storage_item(local_a, "token").unwrap(),
            Some("a-local".to_owned())
        );
        assert_eq!(
            store.get_storage_item(session_a, "token").unwrap(),
            Some("a-session".to_owned())
        );
        assert_eq!(
            store.get_storage_item(local_b, "token").unwrap(),
            Some("b-local".to_owned())
        );
    }

    #[test]
    fn web_storage_clear_and_remove_are_partition_scoped() {
        let (_f, store) = fresh_store();
        let a = "storage:local:https://a.test:443";
        let b = "storage:local:https://b.test:443";

        store.put_storage_item(a, "one", "1").unwrap();
        store.put_storage_item(a, "two", "2").unwrap();
        store.put_storage_item(b, "one", "other").unwrap();

        store.remove_storage_item(a, "one").unwrap();
        assert_eq!(store.get_storage_item(a, "one").unwrap(), None);
        assert_eq!(
            store.get_storage_item(b, "one").unwrap(),
            Some("other".to_owned())
        );

        store.clear_storage_partition(a).unwrap();
        assert!(store.storage_entries(a).unwrap().is_empty());
        assert_eq!(
            store.storage_entries(b).unwrap(),
            vec![("one".to_owned(), "other".to_owned())]
        );
    }

    #[test]
    fn web_storage_rejects_ambiguous_keys() {
        let (_f, store) = fresh_store();
        let err = store
            .put_storage_item("storage:local:https://a.test:443", "", "value")
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidWebStorageInput { field: "key", .. }
        ));

        let err = store
            .put_storage_item("storage:local:https://a.test:443", "bad\0key", "value")
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidWebStorageInput { field: "key", .. }
        ));
    }

    #[test]
    fn web_storage_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage.redb");
        let partition = "storage:local:https://persist.test:443";
        {
            let store = Store::open(&path).unwrap();
            store
                .put_storage_item(partition, "mode", "persisted")
                .unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.get_storage_item(partition, "mode").unwrap(),
            Some("persisted".to_owned())
        );
    }

    #[test]
    fn partition_prefix_boundaries_do_not_bleed() {
        let (_f, store) = fresh_store();
        store
            .put_cookie("https://a.test:443", &cookie("sid", "a"))
            .unwrap();
        store
            .put_cookie("https://a.test:443.evil", &cookie("sid", "evil"))
            .unwrap();
        assert_eq!(store.cookies_for("https://a.test:443").unwrap().len(), 1);
        assert_eq!(
            store.cookies_for("https://a.test:443").unwrap()[0].value,
            "a"
        );

        store
            .put_storage_item("storage:local:https://a.test:443", "sid", "a")
            .unwrap();
        store
            .put_storage_item("storage:local:https://a.test:443.evil", "sid", "evil")
            .unwrap();
        assert_eq!(
            store
                .storage_entries("storage:local:https://a.test:443")
                .unwrap(),
            vec![("sid".to_owned(), "a".to_owned())]
        );
    }

    #[test]
    fn history_appends_visits() {
        let (_f, store) = fresh_store();
        store
            .record_visit("https://a.test:443", "https://a.test/", 100)
            .unwrap();
        store
            .record_visit("https://a.test:443", "https://a.test/", 200)
            .unwrap();
        store
            .record_visit("https://a.test:443", "https://a.test/", 300)
            .unwrap();
        let visits = store
            .visits_for("https://a.test:443", "https://a.test/")
            .unwrap();
        assert_eq!(visits, vec![100, 200, 300]);
        // Partitioned.
        assert!(
            store
                .visits_for("https://b.test:443", "https://a.test/")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn session_round_trips() {
        let (_f, store) = fresh_store();
        let tabs = vec!["https://a.test/".to_owned(), "https://b.test/".to_owned()];
        store.save_session(&tabs).unwrap();
        assert_eq!(store.load_session().unwrap(), tabs);
        assert_eq!(store.load_session_record().unwrap().active_index, 0);
        // Empty store → empty session.
        let (_f2, store2) = fresh_store();
        assert!(store2.load_session().unwrap().is_empty());
    }

    #[test]
    fn session_record_round_trips_active_tab() {
        let (_f, store) = fresh_store();
        let record = SessionRecord {
            tabs: vec![
                "https://one.test/".to_owned(),
                "https://two.test/".to_owned(),
                "https://three.test/".to_owned(),
            ],
            active_index: 1,
            tab_states: Vec::new(),
        };

        store.save_session_record(&record).unwrap();

        assert_eq!(store.load_session_record().unwrap(), record);
        assert_eq!(
            store.load_session().unwrap(),
            vec![
                "https://one.test/".to_owned(),
                "https://two.test/".to_owned(),
                "https://three.test/".to_owned(),
            ]
        );
    }

    #[test]
    fn session_record_round_trips_tab_restore_state() {
        let (_f, store) = fresh_store();
        let record = SessionRecord {
            tabs: vec![
                "https://one.test/form".to_owned(),
                "https://two.test/".to_owned(),
            ],
            active_index: 0,
            tab_states: vec![
                TabSessionState {
                    scroll_x: 12,
                    scroll_y: 345,
                    focused_element: Some("form#login input[name=email]".to_owned()),
                    form_controls: vec![
                        FormControlSessionState {
                            key: "email".to_owned(),
                            value: "user@example.test".to_owned(),
                            checked: None,
                        },
                        FormControlSessionState {
                            key: "remember".to_owned(),
                            value: "on".to_owned(),
                            checked: Some(true),
                        },
                    ],
                },
                TabSessionState::default(),
            ],
        };

        store.save_session_record(&record).unwrap();

        assert_eq!(store.load_session_record().unwrap(), record);
        assert_eq!(
            store.load_session().unwrap(),
            vec![
                "https://one.test/form".to_owned(),
                "https://two.test/".to_owned(),
            ]
        );
    }

    #[test]
    fn session_record_accepts_legacy_record_without_tab_states() {
        let record: SessionRecord =
            decode(br#"{"tabs":["https://legacy.test/"],"active_index":0}"#).unwrap();

        assert_eq!(
            record,
            SessionRecord {
                tabs: vec!["https://legacy.test/".to_owned()],
                active_index: 0,
                tab_states: Vec::new(),
            }
        );
        validate_session_record(&record).unwrap();
    }

    #[test]
    fn session_record_rejects_invalid_restore_state() {
        let (_f, store) = fresh_store();
        let err = store
            .save_session_record(&SessionRecord {
                tabs: vec!["https://one.test/".to_owned()],
                active_index: 1,
                tab_states: Vec::new(),
            })
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidSessionInput {
                field: "active_index",
                ..
            }
        ));

        let err = store
            .save_session_record(&SessionRecord {
                tabs: vec!["bad\0url".to_owned()],
                active_index: 0,
                tab_states: Vec::new(),
            })
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidSessionInput { field: "tabs", .. }
        ));

        let err = store
            .save_session_record(&SessionRecord {
                tabs: vec!["https://one.test/".to_owned()],
                active_index: 0,
                tab_states: vec![TabSessionState::default(), TabSessionState::default()],
            })
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidSessionInput {
                field: "tab_states",
                ..
            }
        ));

        let err = store
            .save_session_record(&SessionRecord {
                tabs: vec!["https://one.test/".to_owned()],
                active_index: 0,
                tab_states: vec![TabSessionState {
                    focused_element: Some("bad\0focus".to_owned()),
                    ..TabSessionState::default()
                }],
            })
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidSessionInput {
                field: "focused_element",
                ..
            }
        ));

        let err = store
            .save_session_record(&SessionRecord {
                tabs: vec!["https://one.test/".to_owned()],
                active_index: 0,
                tab_states: vec![TabSessionState {
                    form_controls: (0..=MAX_SESSION_FORM_CONTROLS)
                        .map(|id| FormControlSessionState {
                            key: format!("field-{id}"),
                            value: String::new(),
                            checked: None,
                        })
                        .collect(),
                    ..TabSessionState::default()
                }],
            })
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidSessionInput {
                field: "form_controls",
                ..
            }
        ));
    }

    #[test]
    fn session_record_is_bounded() {
        let (_f, store) = fresh_store();
        let record = SessionRecord {
            tabs: (0..=MAX_SESSION_TABS)
                .map(|id| format!("https://tab-{id}.test/"))
                .collect(),
            active_index: 0,
            tab_states: Vec::new(),
        };

        let err = store.save_session_record(&record).unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidSessionInput { field: "tabs", .. }
        ));
    }

    #[test]
    fn permissions_round_trip_and_partition_by_origin() {
        let (_f, store) = fresh_store();
        let a = permission(
            "https://a.test:443",
            "notifications",
            PermissionDecision::Granted,
        );
        let b = permission("https://b.test:443", "camera", PermissionDecision::Denied);

        store.put_permission(&a).unwrap();
        store.put_permission(&b).unwrap();

        assert_eq!(
            store
                .permission("https://a.test:443", "notifications")
                .unwrap(),
            Some(a.clone())
        );
        assert_eq!(
            store.permissions_for("https://a.test:443").unwrap(),
            vec![a]
        );
        assert_eq!(
            store.permissions_for("https://b.test:443").unwrap(),
            vec![b]
        );
        assert!(
            store
                .permission("https://a.test:443", "geolocation")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn clear_permissions_is_origin_scoped() {
        let (_f, store) = fresh_store();
        store
            .put_permission(&permission(
                "https://a.test:443",
                "notifications",
                PermissionDecision::Granted,
            ))
            .unwrap();
        store
            .put_permission(&permission(
                "https://b.test:443",
                "notifications",
                PermissionDecision::Denied,
            ))
            .unwrap();

        store.clear_permissions("https://a.test:443").unwrap();

        assert!(
            store
                .permissions_for("https://a.test:443")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.permissions_for("https://b.test:443").unwrap().len(),
            1
        );
    }

    #[test]
    fn permissions_reject_ambiguous_keys() {
        let (_f, store) = fresh_store();
        let err = store
            .put_permission(&permission(
                "",
                "notifications",
                PermissionDecision::Granted,
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidPermissionInput {
                field: "origin_key",
                ..
            }
        ));

        let err = store
            .put_permission(&permission(
                "https://a.test:443",
                "bad\0permission",
                PermissionDecision::Denied,
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidPermissionInput { field: "kind", .. }
        ));
    }

    #[test]
    fn hsts_round_trips_case_insensitive_host_key() {
        let (_f, store) = fresh_store();
        let record = hsts("Example.COM");

        store.put_hsts(&record).unwrap();

        assert_eq!(store.hsts("example.com").unwrap(), Some(record));
    }

    #[test]
    fn hsts_delete_removes_security_state() {
        let (_f, store) = fresh_store();
        store.put_hsts(&hsts("delete.test")).unwrap();

        store.delete_hsts("delete.test").unwrap();

        assert!(store.hsts("delete.test").unwrap().is_none());
    }

    #[test]
    fn hsts_rejects_invalid_records() {
        let (_f, store) = fresh_store();
        let mut invalid = hsts("");
        let err = store.put_hsts(&invalid).unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidSecurityStateInput { field: "host", .. }
        ));

        invalid = hsts("example.test");
        invalid.expires_unix = invalid.received_unix - 1;
        let err = store.put_hsts(&invalid).unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidSecurityStateInput {
                field: "expires_unix",
                ..
            }
        ));
    }

    #[test]
    fn downloads_round_trip_newest_first_and_update_by_id() {
        let (_f, store) = fresh_store();
        store.put_download(&download(1, 100)).unwrap();
        store.put_download(&download(2, 200)).unwrap();

        assert_eq!(
            store
                .downloads()
                .unwrap()
                .into_iter()
                .map(|record| record.id)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );

        let mut updated = download(1, 100);
        updated.received_bytes = 10_000;
        updated.state = DownloadState::Completed;
        updated.updated_unix = 300;
        store.put_download(&updated).unwrap();

        let downloads = store.downloads().unwrap();
        assert_eq!(downloads.len(), 2);
        assert_eq!(downloads[1], updated);
    }

    #[test]
    fn downloads_are_bounded_to_newest_records() {
        let (_f, store) = fresh_store();
        let extra = 3;
        for id in 0..(MAX_DOWNLOAD_RECORDS as u64 + extra) {
            store.put_download(&download(id, id as i64)).unwrap();
        }

        let downloads = store.downloads().unwrap();
        assert_eq!(downloads.len(), MAX_DOWNLOAD_RECORDS);
        assert_eq!(
            downloads.first().unwrap().id,
            MAX_DOWNLOAD_RECORDS as u64 + extra - 1
        );
        assert_eq!(downloads.last().unwrap().id, extra);
    }

    #[test]
    fn clear_downloads_removes_history() {
        let (_f, store) = fresh_store();
        store.put_download(&download(1, 100)).unwrap();
        store.put_download(&download(2, 200)).unwrap();

        store.clear_downloads().unwrap();

        assert!(store.downloads().unwrap().is_empty());
    }

    #[test]
    fn clear_profile_data_removes_selected_groups_and_preserves_session() {
        let (_f, store) = fresh_store();
        populate_profile_data(&store);

        store
            .clear_profile_data(ClearDataSelection::browsing_data())
            .unwrap();

        assert!(
            store
                .cookies_for("https://clear.test:443")
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .get_cache("https://clear.test:443", "https://clear.test/page")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .cache_alias("https://clear.test:443", "https://clear.test/old")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .visits_for("https://clear.test:443", "https://clear.test/page")
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .storage_entries("storage:local:https://clear.test:443")
                .unwrap()
                .is_empty()
        );
        assert!(store.downloads().unwrap().is_empty());
        assert!(
            store
                .permissions_for("https://clear.test:443")
                .unwrap()
                .is_empty()
        );
        assert!(store.hsts("clear.test").unwrap().is_none());
        assert_eq!(
            store.load_session().unwrap(),
            vec!["https://clear.test/page".to_owned()]
        );
    }

    #[test]
    fn clear_profile_data_all_removes_session_restore_too() {
        let (_f, store) = fresh_store();
        populate_profile_data(&store);

        store.clear_profile_data(ClearDataSelection::all()).unwrap();

        assert!(store.load_session().unwrap().is_empty());
        assert!(
            store
                .cookies_for("https://clear.test:443")
                .unwrap()
                .is_empty()
        );
        assert!(store.downloads().unwrap().is_empty());
    }

    #[test]
    fn downloads_reject_invalid_records() {
        let (_f, store) = fresh_store();
        let mut invalid = download(1, 100);
        invalid.filename.clear();
        let err = store.put_download(&invalid).unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidDownloadInput {
                field: "filename",
                ..
            }
        ));

        let mut invalid = download(1, 100);
        invalid.received_bytes = 10;
        invalid.total_bytes = Some(9);
        let err = store.put_download(&invalid).unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidDownloadInput {
                field: "received_bytes",
                ..
            }
        ));

        let mut invalid = download(1, 100);
        invalid.destination_path = Some("/bad\0path".to_owned());
        let err = store.put_download(&invalid).unwrap_err();
        assert!(matches!(
            err,
            StoreError::InvalidDownloadInput {
                field: "destination_path",
                ..
            }
        ));
    }

    #[test]
    fn downloads_persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("downloads.redb");
        let record = download(42, 1_000);
        {
            let store = Store::open(&path).unwrap();
            store.put_download(&record).unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(store.downloads().unwrap(), vec![record]);
    }

    #[test]
    fn store_persists_across_reopen() {
        // Data must survive closing and reopening the database file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("persist.redb");
        {
            let store = Store::open(&path).unwrap();
            store
                .put_cookie("https://a.test:443", &cookie("sid", "persisted"))
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let got = store.cookies_for("https://a.test:443").unwrap();
        assert_eq!(got[0].value, "persisted");
    }
}
