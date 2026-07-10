//! Permissions API — small per-origin permission store.
//!
//! docs/SPEC.md mentions the Permissions API under the security boundaries;
//! full enforcement lands at Phase 7. Phase 1 provides the types and a
//! fail-closed in-memory store (default state is `Prompt`, and `Prompt`
//! is treated as **denied** until the shell/user grants it).

use std::collections::HashMap;

use crate::origin::Origin;

/// Permission kinds a page may query/request at v1.0. Mirrors the
/// `vixen_api::Permission` shape but is duplicated here on purpose:
/// `vixen-net` deliberately depends on no other vixen crate (see
/// `Cargo.toml` and docs/ARCHITECTURE.md "Dependency direction").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionKind {
    Geolocation,
    Notifications,
    Camera,
    Microphone,
    ClipboardRead,
    PersistentStorage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionState {
    /// Granted explicitly. The shell records a user grant here.
    Granted,
    /// Denied explicitly or by policy.
    Denied,
    /// Default; treated as denied until the user grants it (fail closed).
    Prompt,
}

/// In-memory per-origin permission store. Persisted form lands with
/// `vixen-store`; this is the authoritative in-process cache.
#[derive(Debug, Default)]
pub struct PermissionStore {
    entries: HashMap<(Origin, PermissionKind), PermissionState>,
}

impl PermissionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current state for `(origin, kind)`. Unknown → `Prompt`.
    pub fn state(&self, origin: &Origin, kind: PermissionKind) -> PermissionState {
        self.entries
            .get(&(origin.clone(), kind))
            .copied()
            .unwrap_or(PermissionState::Prompt)
    }

    /// Record an explicit decision.
    pub fn set(&mut self, origin: Origin, kind: PermissionKind, state: PermissionState) {
        self.entries.insert((origin, kind), state);
    }

    /// Convenience: is `kind` granted for `origin`? `Prompt` is **not** a
    /// grant — callers must treat prompt as denied (fail closed).
    pub fn is_granted(&self, origin: &Origin, kind: PermissionKind) -> bool {
        self.state(origin, kind) == PermissionState::Granted
    }

    /// Forget a decision (e.g. site data cleared). Returns to `Prompt`.
    pub fn revoke(&mut self, origin: &Origin, kind: PermissionKind) {
        self.entries.remove(&(origin.clone(), kind));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn o(s: &str) -> Origin {
        Origin::from_url(&Url::parse(s).unwrap())
    }

    #[test]
    fn unknown_defaults_to_prompt_and_is_not_granted() {
        let store = PermissionStore::new();
        let origin = o("https://a.test");
        assert_eq!(
            store.state(&origin, PermissionKind::Geolocation),
            PermissionState::Prompt
        );
        assert!(!store.is_granted(&origin, PermissionKind::Geolocation));
    }

    #[test]
    fn grant_is_persisted_per_origin() {
        let mut store = PermissionStore::new();
        let a = o("https://a.test");
        let b = o("https://b.test");
        store.set(
            a.clone(),
            PermissionKind::Notifications,
            PermissionState::Granted,
        );
        assert!(store.is_granted(&a, PermissionKind::Notifications));
        // Different origin is unaffected.
        assert!(!store.is_granted(&b, PermissionKind::Notifications));
    }

    #[test]
    fn revoke_returns_to_prompt() {
        let mut store = PermissionStore::new();
        let origin = o("https://a.test");
        store.set(
            origin.clone(),
            PermissionKind::Camera,
            PermissionState::Denied,
        );
        store.revoke(&origin, PermissionKind::Camera);
        assert_eq!(
            store.state(&origin, PermissionKind::Camera),
            PermissionState::Prompt
        );
    }
}
