//! vixen-store — redb-backed persistence.
//!
//! Per-origin partitioned storage for cookies, fetch cache, history, and
//! sessions (docs/ARCHITECTURE.md "App ID and profile paths"). The crate is
//! deliberately independent of `vixen-net`: callers pass an opaque
//! `origin_key` (e.g. an `Origin::partition_key()`) so store never depends
//! on networking. Every table namespaces by that key so cross-origin reads
//! are impossible (docs/SPEC.md origin isolation).

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
const HISTORY: TableDefinition<&[u8], &[u8]> = TableDefinition::new("history");
const SESSION: TableDefinition<&[u8], &[u8]> = TableDefinition::new("session");
const SESSION_KEY: &[u8] = b"open-tabs";

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
            let _ = w.open_table(HISTORY)?;
            let _ = w.open_table(SESSION)?;
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
        let prefix = origin_key.as_bytes();
        let mut out = Vec::new();
        for item in t.iter()? {
            let (k, v) = item?;
            let k = k.value();
            if !k.starts_with(prefix) {
                continue;
            }
            // Strip the `<origin_key>\x00` separator before the name.
            if let Some(idx) = k.iter().position(|&b| b == 0) {
                let _name = &k[idx + 1..];
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

    // --- Fetch cache --------------------------------------------------------

    pub fn put_cache(&self, origin_key: &str, url: &str, entry: &CacheEntry) -> Result<()> {
        let key = namespaced_key(origin_key, url);
        let val = encode(entry)?;
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(FETCH_CACHE)?;
            t.insert(key.as_slice(), val.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    pub fn get_cache(&self, origin_key: &str, url: &str) -> Result<Option<CacheEntry>> {
        let r = self.db.begin_read()?;
        let t = r
            .open_table(FETCH_CACHE)
            .map_err(|_| StoreError::MissingTable("fetch-cache"))?;
        let key = namespaced_key(origin_key, url);
        match t.get(key.as_slice())? {
            Some(v) => Ok(Some(decode(v.value())?)),
            None => Ok(None),
        }
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
        let val = encode(tabs)?;
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(SESSION)?;
            t.insert(SESSION_KEY, val.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    pub fn load_session(&self) -> Result<Vec<String>> {
        let r = self.db.begin_read()?;
        let t = r
            .open_table(SESSION)
            .map_err(|_| StoreError::MissingTable("session"))?;
        match t.get(SESSION_KEY)? {
            Some(v) => Ok(decode(v.value())?),
            None => Ok(Vec::new()),
        }
    }
}

fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(StoreError::from)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(StoreError::from)
}

/// Build `<origin_key> \x00 <name>` so origin partitions never collide.
fn namespaced_key(origin_key: &str, name: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(origin_key.len() + 1 + name.len());
    k.extend_from_slice(origin_key.as_bytes());
    k.push(0);
    k.extend_from_slice(name.as_bytes());
    k
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
    fn fetch_cache_round_trips() {
        let (_f, store) = fresh_store();
        let entry = CacheEntry {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: b"<html></html>".to_vec(),
            fetched_unix: 1_234,
        };
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
        // Empty store → empty session.
        let (_f2, store2) = fresh_store();
        assert!(store2.load_session().unwrap().is_empty());
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
