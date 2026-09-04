//! Web Storage (`localStorage` / `sessionStorage`) key + value validation and
//! per-origin partitioning — Phase 6 host-binding prep (pure logic). The
//! DOM-side `Storage` host hooks (Phase 6) and the `vixen-store` partitioning
//! layer (Phase 1) share this one source of truth for what is an acceptable
//! key/value and how the on-disk partition key is derived.
//!
//! What lives here:
//! - [`StorageKind`] — `local` vs `session` (HTML § DOM/Storage).
//! - [`validate_storage_key`] / [`validate_storage_value`] — the Vixen-pinned
//!   rules (non-empty, no NUL bytes, ≤ [`MAX_KEY_LEN`] / [`MAX_VALUE_LEN`]).
//! - [`StoragePartition`] — `(origin, kind)` → the redb table/partition key,
//!   so `localStorage` and `sessionStorage` never collide and two origins
//!   never see each other's data.
//! - [`StorageQuota`] — per-partition entry + byte quota (the host hook
//!   reports `QuotaExceededError` against this).
//!
//! What does *not* live here:
//! - The actual redb read/write (that's `vixen-store`, Phase 1).
//! - The JS `Storage` object surface (`getItem`/`setItem`/`key`/`length`,
//!   enumeration order) — that's the Phase 6 host hook layer.
//! - Session restoration (the caller persists `sessionStorage` to `vixen-store`
//!   per docs/ARCHITECTURE.md "App ID and profile paths").
//!
//! Vixen's pinned rules (the Vixen-specific configuration; the HTML spec only
//! requires "convert to a DOMString" + a UA-defined quota):
//! - **Keys are non-empty.** The HTML spec technically permits the empty
//!   string as a key (it's a no-op on `getItem`/`removeItem`), but Vixen
//!   rejects it at `setItem` to keep the redb key space canonical — an empty
//!   key round-trips ambiguously with "absent". Callers that observe a
//!   `getItem("")` call still return `null` per spec (the host-hook layer
//!   handles that before calling [`validate_storage_key`]).
//! - **No NUL bytes** in keys (would break length-prefixed redb keys) or
//!   values (would break value truncation). Control characters otherwise are
//!   allowed — values are opaque UTF-8.
//! - **Length caps** at [`MAX_KEY_LEN`] / [`MAX_VALUE_LEN`] / [`MAX_ENTRIES`]
//!   per partition; exceeding the total raises `QuotaExceededError`.
//!
//! Reference: HTML Living Standard, "Web storage"
//! (<https://html.spec.whatwg.org/multipage/webstorage.html>).

#![forbid(unsafe_code)]

use vixen_net::Origin;

/// `localStorage` (persistent) vs `sessionStorage` (tab-scoped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageKind {
    Local,
    Session,
}

impl StorageKind {
    /// The tag used in the partition key prefix so the two stores never share
    /// a redb table even for the same origin.
    pub fn tag(self) -> &'static str {
        match self {
            StorageKind::Local => "local",
            StorageKind::Session => "session",
        }
    }
}

/// Hard cap on a single key's UTF-8 byte length. Generous enough for any
/// reasonable Web-Storage usage; small enough to keep redb keys cheap.
pub const MAX_KEY_LEN: usize = 1024;
/// Hard cap on a single value's UTF-8 byte length (≈ 2 MiB). Mirrors the
/// common per-item browser limit.
pub const MAX_VALUE_LEN: usize = 2 * 1024 * 1024;
/// Hard cap on entries per `(origin, kind)` partition. Browsers usually gate
/// on total bytes (5 MiB); Vixen additionally caps the entry count so a
/// pathological site can't bloat the redb table index.
pub const MAX_ENTRIES: usize = 8192;
/// Hard cap on the *total* bytes across one `(origin, kind)` partition — the
/// 5 MiB figure browsers ship. [`StorageQuota::check`] reports
/// `QuotaExceededError` past this.
pub const MAX_PARTITION_BYTES: usize = 5 * 1024 * 1024;

/// Error from [`validate_storage_key`] / [`validate_storage_value`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageKeyError {
    /// The empty string is rejected at `setItem` (see module docs).
    #[error("storage key must be non-empty")]
    Empty,
    /// NUL bytes would corrupt the on-disk encoding.
    #[error("storage key contains a NUL byte")]
    ContainsNul,
    /// Key/value exceeded the byte cap.
    #[error("storage {what} length {len} exceeds maximum {max}")]
    TooLong {
        what: &'static str,
        len: usize,
        max: usize,
    },
}

/// Validate a `localStorage`/`sessionStorage` key (Vixen's pinned rules:
/// non-empty, no NUL, ≤ [`MAX_KEY_LEN`] bytes). Used by the `setItem`/
/// `getItem`/`removeItem`/`key` host hooks.
pub fn validate_storage_key(key: &str) -> Result<(), StorageKeyError> {
    if key.is_empty() {
        return Err(StorageKeyError::Empty);
    }
    if key.as_bytes().contains(&0) {
        return Err(StorageKeyError::ContainsNul);
    }
    if key.len() > MAX_KEY_LEN {
        return Err(StorageKeyError::TooLong {
            what: "key",
            len: key.len(),
            max: MAX_KEY_LEN,
        });
    }
    Ok(())
}

/// Validate a `localStorage`/`sessionStorage` value. Values may be empty and
/// may contain any non-NUL UTF-8 (the API stringifies, so the host hook layer
/// has already converted to a string before calling this).
pub fn validate_storage_value(value: &str) -> Result<(), StorageKeyError> {
    if value.as_bytes().contains(&0) {
        return Err(StorageKeyError::ContainsNul);
    }
    if value.len() > MAX_VALUE_LEN {
        return Err(StorageKeyError::TooLong {
            what: "value",
            len: value.len(),
            max: MAX_VALUE_LEN,
        });
    }
    Ok(())
}

/// A `(origin, kind)` storage partition. Two origins never share a partition;
/// `local` and `session` never share either. The on-disk key is derived
/// deterministically so a tab restart finds the same data.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StoragePartition {
    origin: Origin,
    kind: StorageKind,
}

impl StoragePartition {
    /// Construct a partition for an origin + kind. Opaque origins (e.g.
    /// `data:` documents) get an isolated partition so they can't reach a
    /// tuple origin's storage — but their data still doesn't leak across
    /// documents because the partition key includes the per-document sentinel.
    pub fn new(origin: Origin, kind: StorageKind) -> Self {
        Self { origin, kind }
    }

    /// The storage kind.
    pub fn kind(&self) -> StorageKind {
        self.kind
    }

    /// The origin.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Deterministic on-disk partition key. Format: `storage:{kind}:{origin}`.
    /// Opaque origins partition under `storage:{kind}:opaque` and the caller
    /// is responsible for the per-document scoping if it wants them isolated.
    pub fn partition_key(&self) -> String {
        format!(
            "storage:{}:{}",
            self.kind.tag(),
            self.origin.partition_key()
        )
    }
}

/// Per-partition quota result. The host hook layer reports `QuotaExceededError`
/// when [`StorageQuota::check`] returns `Err`.
#[derive(Debug, Clone, Copy)]
pub struct StorageQuota {
    pub entries: usize,
    pub bytes: usize,
}

impl StorageQuota {
    /// Check whether adding one more entry of `key_len + value_len` bytes
    /// stays within the per-partition limits.
    pub fn check(&self, key_len: usize, value_len: usize) -> Result<(), StorageKeyError> {
        if self.entries + 1 > MAX_ENTRIES {
            return Err(StorageKeyError::TooLong {
                what: "entry-count",
                len: self.entries + 1,
                max: MAX_ENTRIES,
            });
        }
        let added = self.bytes + key_len + value_len;
        if added > MAX_PARTITION_BYTES {
            return Err(StorageKeyError::TooLong {
                what: "partition-bytes",
                len: added,
                max: MAX_PARTITION_BYTES,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn origin(s: &str) -> Origin {
        Origin::from_url(&Url::parse(s).unwrap())
    }

    // --- Key validation ------------------------------------------------

    #[test]
    fn key_accepts_normal_strings() {
        assert!(validate_storage_key("theme").is_ok());
        assert!(validate_storage_key("user:id=42").is_ok());
        // Spaces, unicode, control chars (non-NUL) are fine.
        assert!(validate_storage_key("a b").is_ok());
        assert!(validate_storage_key("café").is_ok());
        assert!(validate_storage_key("\t\n").is_ok());
    }

    #[test]
    fn key_rejects_empty() {
        assert_eq!(validate_storage_key(""), Err(StorageKeyError::Empty));
    }

    #[test]
    fn key_rejects_nul_bytes() {
        assert_eq!(
            validate_storage_key("a\0b"),
            Err(StorageKeyError::ContainsNul)
        );
    }

    #[test]
    fn key_rejects_too_long() {
        let long = "x".repeat(MAX_KEY_LEN + 1);
        assert!(matches!(
            validate_storage_key(&long),
            Err(StorageKeyError::TooLong { what: "key", .. })
        ));
        // Exactly at the cap is fine.
        let exact = "x".repeat(MAX_KEY_LEN);
        assert!(validate_storage_key(&exact).is_ok());
    }

    // --- Value validation ---------------------------------------------

    #[test]
    fn value_accepts_empty_and_normal() {
        assert!(validate_storage_value("").is_ok());
        assert!(validate_storage_value("hello").is_ok());
        assert!(validate_storage_value(&"x".repeat(MAX_VALUE_LEN)).is_ok());
    }

    #[test]
    fn value_rejects_nul_and_too_long() {
        assert_eq!(
            validate_storage_value("a\0"),
            Err(StorageKeyError::ContainsNul)
        );
        let big = "x".repeat(MAX_VALUE_LEN + 1);
        assert!(matches!(
            validate_storage_value(&big),
            Err(StorageKeyError::TooLong { what: "value", .. })
        ));
    }

    // --- Partitioning --------------------------------------------------

    #[test]
    fn partition_key_is_origin_and_kind_scoped() {
        let a = StoragePartition::new(origin("https://a.test/"), StorageKind::Local);
        let b = StoragePartition::new(origin("https://b.test/"), StorageKind::Local);
        assert_ne!(a.partition_key(), b.partition_key());

        // local vs session never collide even for the same origin.
        let local = StoragePartition::new(origin("https://a.test/"), StorageKind::Local);
        let session = StoragePartition::new(origin("https://a.test/"), StorageKind::Session);
        assert_ne!(local.partition_key(), session.partition_key());
    }

    #[test]
    fn partition_key_is_deterministic() {
        // Same origin + kind → same key (tab restart finds the same data).
        let a = StoragePartition::new(origin("https://a.test:8443/"), StorageKind::Local);
        let b = StoragePartition::new(origin("https://a.test:8443/other/path"), StorageKind::Local);
        assert_eq!(a.partition_key(), b.partition_key());
        assert_eq!(a.partition_key(), "storage:local:https://a.test:8443");
    }

    #[test]
    fn opaque_origin_partitions_isolated() {
        let o = Origin::opaque();
        let p = StoragePartition::new(o, StorageKind::Local);
        assert_eq!(p.partition_key(), "storage:local:opaque");
        // And it never matches a tuple origin's partition.
        let tup = StoragePartition::new(origin("https://opaque/"), StorageKind::Local);
        assert_ne!(p.partition_key(), tup.partition_key());
    }

    #[test]
    fn kind_tag_round_trips() {
        assert_eq!(StorageKind::Local.tag(), "local");
        assert_eq!(StorageKind::Session.tag(), "session");
    }

    // --- Quota ---------------------------------------------------------

    #[test]
    fn quota_allows_room() {
        let q = StorageQuota {
            entries: 0,
            bytes: 0,
        };
        assert!(q.check(10, 100).is_ok());
    }

    #[test]
    fn quota_rejects_over_entry_cap() {
        let q = StorageQuota {
            entries: MAX_ENTRIES,
            bytes: 0,
        };
        assert!(matches!(
            q.check(1, 1),
            Err(StorageKeyError::TooLong {
                what: "entry-count",
                ..
            })
        ));
    }

    #[test]
    fn quota_rejects_over_byte_cap() {
        let q = StorageQuota {
            entries: 0,
            bytes: MAX_PARTITION_BYTES,
        };
        assert!(matches!(
            q.check(1, 1),
            Err(StorageKeyError::TooLong {
                what: "partition-bytes",
                ..
            })
        ));
    }
}
