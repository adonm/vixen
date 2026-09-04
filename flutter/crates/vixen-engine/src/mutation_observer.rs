//! DOM § 4.3 `MutationObserver` + `MutationRecord` — the mutation-queue
//! model the `MutationObserver` host hook + the microtask-delivery step
//! reduce to (Phase 6 DOM prep). Pure given the observation registrations
//! and the per-mutation relation; the live DOM tree walk that classifies a
//! mutation's relation to each observed root is the host hook.
//!
//! What lives here:
//! - [`MutationType`] — § 4.3.2 the three record types (`childList` /
//!   `attributes` / `characterData`).
//! - [`MutationRecord`] — § 4.3.2 one record: the type + target + the
//!   `addedNodes` / `removedNodes` / `previousSibling` / `nextSibling`
//!   (childList) + `attributeName` / `attributeNamespace` (attributes) +
//!   `oldValue`.
//! - [`MutationObserverInit`] — § 4.3.1 the `observe()` options
//!   (`childList` / `attributes` / `attributeFilter` /
//!   `attributeOldValue` / `characterData` / `characterDataOldValue` /
//!   `subtree`).
//! - [`Relation`] — whether a mutation's target is the observed root
//!   itself or a descendant (the host hook walks the tree to classify).
//! - [`should_observe`] — the § 4.3.1 match predicate (the options vs the
//!   mutation type + the relation + the attribute filter).
//! - [`MutationObserver`] — the record queue + the observation
//!   registrations + `observe` / `disconnect` / `takeRecords` /
//!   `drain_for_delivery`.
//!
//! What does *not* live here:
//! - The live DOM tree walk that classifies a mutation's relation to each
//!   observed root — the host hook walks the tree; this module takes the
//!   [`Relation`] as input.
//! - The microtask checkpoint scheduling — the event-loop layer runs the
//!   checkpoint; [`MutationObserver::drain_for_delivery`] is the pure
//!   "batch this observer's pending records into one delivery" step the
//!   checkpoint calls.
//! - The callback invocation — the host hook calls the JS callback with
//!   the drained `Vec<MutationRecord>`; this module produces the Vec.
//! - The § 4.3.2 "transient registration" propagation (a subtree
//!   observation that lifts to a descendant's subtree when that descendant
//!   is itself observed) — v1.0 models the direct + subtree observation;
//!   the transient-registration propagation lands with the host hook.
//!
//! ## The match predicate
//!
//! A mutation of `type` on a node with `relation` to an observed root + the
//! `attribute_name` (for attributes) is observed iff:
//!
//! ```text
//! childList      : options.child_list ∧ (Target ∨ (Descendant ∧ options.subtree))
//! attributes     : options.attributes ∧ (Target ∨ (Descendant ∧ options.subtree))
//!                  ∧ (attributeFilter empty ∨ contains attribute_name)
//! characterData  : options.character_data ∧ (Target ∨ (Descendant ∧ options.subtree))
//! ```
//!
//! Reference: <https://dom.spec.whatwg.org/#interface-mutationobserver>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// NodeHandle + MutationType + MutationRecord
// ---------------------------------------------------------------------------

/// An opaque DOM-node handle (the host hook's table key). Reused across the
/// DOM-prep modules; kept local so this module stays self-contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeHandle(pub usize);

/// DOM § 4.3.2 the `MutationRecord.type` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MutationType {
    /// `childList` — the `childNodes` list of the target changed.
    ChildList,
    /// `attributes` — an attribute of the target changed.
    Attributes,
    /// `characterData` — the `data` of a text/comment node changed.
    CharacterData,
}

impl MutationType {
    /// The serialised `MutationRecord.type` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChildList => "childList",
            Self::Attributes => "attributes",
            Self::CharacterData => "characterData",
        }
    }
}

/// DOM § 4.3.2 one `MutationRecord`. The fields are sparsely populated per
/// the `type`: `addedNodes`/`removedNodes`/`previousSibling`/`nextSibling`
/// for `childList`; `attributeName`/`attributeNamespace` for `attributes`;
/// `oldValue` for `attributes`/`characterData` when the corresponding
/// `*OldValue` option was set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRecord {
    /// The record type.
    pub type_: MutationType,
    /// The target node the mutation affected.
    pub target: NodeHandle,
    /// The added child nodes (`childList` only).
    pub added_nodes: Vec<NodeHandle>,
    /// The removed child nodes (`childList` only).
    pub removed_nodes: Vec<NodeHandle>,
    /// The previous sibling of the added/removed nodes (`childList`; may
    /// be `None`).
    pub previous_sibling: Option<NodeHandle>,
    /// The next sibling of the added/removed nodes (`childList`; may be
    /// `None`).
    pub next_sibling: Option<NodeHandle>,
    /// The local name of the changed attribute (`attributes` only).
    pub attribute_name: Option<String>,
    /// The namespace of the changed attribute (`attributes` only).
    pub attribute_namespace: Option<String>,
    /// The old value (`attributes` if `attributeOldValue`; `characterData`
    /// if `characterDataOldValue`).
    pub old_value: Option<String>,
}

impl MutationRecord {
    /// Construct a `childList` record.
    pub fn child_list(
        target: NodeHandle,
        added: Vec<NodeHandle>,
        removed: Vec<NodeHandle>,
        previous_sibling: Option<NodeHandle>,
        next_sibling: Option<NodeHandle>,
    ) -> Self {
        Self {
            type_: MutationType::ChildList,
            target,
            added_nodes: added,
            removed_nodes: removed,
            previous_sibling,
            next_sibling,
            attribute_name: None,
            attribute_namespace: None,
            old_value: None,
        }
    }

    /// Construct an `attributes` record.
    pub fn attribute(
        target: NodeHandle,
        name: impl Into<String>,
        namespace: Option<String>,
        old_value: Option<String>,
    ) -> Self {
        Self {
            type_: MutationType::Attributes,
            target,
            added_nodes: vec![],
            removed_nodes: vec![],
            previous_sibling: None,
            next_sibling: None,
            attribute_name: Some(name.into()),
            attribute_namespace: namespace,
            old_value,
        }
    }

    /// Construct a `characterData` record.
    pub fn character_data(target: NodeHandle, old_value: Option<String>) -> Self {
        Self {
            type_: MutationType::CharacterData,
            target,
            added_nodes: vec![],
            removed_nodes: vec![],
            previous_sibling: None,
            next_sibling: None,
            attribute_name: None,
            attribute_namespace: None,
            old_value,
        }
    }
}

// ---------------------------------------------------------------------------
// MutationObserverInit + Relation + should_observe
// ---------------------------------------------------------------------------

/// DOM § 4.3.1 the `MutationObserver.observe()` options. Defaults match the
/// spec: every boolean defaults to `false`; `attributeFilter` defaults to
/// `None` (observe every attribute when `attributes` is true).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MutationObserverInit {
    /// `childList` — observe child-tree changes on the target.
    pub child_list: bool,
    /// `attributes` — observe attribute changes on the target.
    pub attributes: bool,
    /// `attributeFilter` — the restricted attribute-name set (`None` ⇒ all
    /// attributes).
    pub attribute_filter: Option<Vec<String>>,
    /// `attributeOldValue` — record the previous attribute value.
    pub attribute_old_value: bool,
    /// `characterData` — observe `data` changes on text/comment targets.
    pub character_data: bool,
    /// `characterDataOldValue` — record the previous `data` value.
    pub character_data_old_value: bool,
    /// `subtree` — extend the observation to the target's descendants.
    pub subtree: bool,
}

impl MutationObserverInit {
    /// The § 4.3.1 "at least one of childList/attributes/characterData
    /// must be true" validity check (the host hook rejects an empty
    /// options bag with a `TypeError`).
    pub fn is_valid(&self) -> bool {
        self.child_list || self.attributes || self.character_data
    }

    /// `true` iff `attribute_filter` is empty/`None` (observe all
    /// attributes) or contains `name`.
    pub fn allows_attribute(&self, name: &str) -> bool {
        match &self.attribute_filter {
            None => true,
            Some(list) => list.iter().any(|n| n == name),
        }
    }
}

/// The relation of a mutation's target to an observed root (the host hook
/// walks the tree to classify).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Relation {
    /// The mutation target is the observed root itself.
    Target,
    /// The mutation target is a descendant of the observed root (only
    /// observable when `subtree` is true).
    Descendant,
}

/// The § 4.3.1 match predicate: `true` iff a mutation of `type_` with
/// `attribute_name` (for attributes) on a node with `relation` to an
/// observed root configured with `options` should produce a record.
pub fn should_observe(
    relation: Relation,
    type_: MutationType,
    attribute_name: Option<&str>,
    options: &MutationObserverInit,
) -> bool {
    let in_scope = match relation {
        Relation::Target => true,
        Relation::Descendant => options.subtree,
    };
    if !in_scope {
        return false;
    }
    match type_ {
        MutationType::ChildList => options.child_list,
        MutationType::Attributes => {
            options.attributes && options.allows_attribute(attribute_name.unwrap_or(""))
        }
        MutationType::CharacterData => options.character_data,
    }
}

// ---------------------------------------------------------------------------
// MutationObserver
// ---------------------------------------------------------------------------

/// DOM § 4.3 `MutationObserver`: the record queue + the observation
/// registrations. The host hook constructs one per JS `MutationObserver`,
/// enqueues records as mutations happen, and drains the queue at the
/// microtask checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MutationObserver {
    records: Vec<MutationRecord>,
    registrations: Vec<(NodeHandle, MutationObserverInit)>,
}

impl MutationObserver {
    /// Construct an empty observer.
    pub fn new() -> Self {
        Self::default()
    }

    /// `observe(target, options)` — register (or replace) an observation on
    /// `target`. Re-observing the same target replaces the prior options
    /// per § 4.3.1. The options are validated first (`is_valid`); an
    /// invalid bag is rejected (`false` returned, no registration added).
    pub fn observe(&mut self, target: NodeHandle, options: MutationObserverInit) -> bool {
        if !options.is_valid() {
            return false;
        }
        if let Some(slot) = self.registrations.iter_mut().find(|(t, _)| *t == target) {
            slot.1 = options;
        } else {
            self.registrations.push((target, options));
        }
        true
    }

    /// `disconnect()` — remove every observation registration. The pending
    /// record queue is left intact (the spec's `disconnect` does not clear
    /// already-queued records; use [`Self::take_records`] for that).
    pub fn disconnect(&mut self) {
        self.registrations.clear();
    }

    /// The registered observations (the host hook's `observe` surface).
    pub fn registrations(&self) -> &[(NodeHandle, MutationObserverInit)] {
        &self.registrations
    }

    /// Enqueue a record (the host hook calls this once it has determined
    /// the mutation matches one of this observer's registrations via
    /// [`should_observe`]).
    pub fn enqueue(&mut self, record: MutationRecord) {
        self.records.push(record);
    }

    /// `takeRecords()` — drain + return the pending record queue (the
    /// synchronous "give me the records so far" surface).
    pub fn take_records(&mut self) -> Vec<MutationRecord> {
        std::mem::take(&mut self.records)
    }

    /// The microtask-checkpoint delivery: drain + return the batched
    /// records the host hook passes to the JS callback in one call (the
    /// § 4.3 "notify mutation observers" step). Identical to
    /// [`Self::take_records`] at the pure layer; the event-loop layer
    /// ensures this runs once per microtask checkpoint per observer.
    pub fn drain_for_delivery(&mut self) -> Vec<MutationRecord> {
        self.take_records()
    }

    /// The number of pending records.
    pub fn pending_count(&self) -> usize {
        self.records.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn n(i: usize) -> NodeHandle {
        NodeHandle(i)
    }

    // --- MutationRecord constructors ---------------------------------

    #[test]
    fn child_list_record_carries_added_removed_siblings() {
        let r = MutationRecord::child_list(n(1), vec![n(2)], vec![n(3)], Some(n(0)), Some(n(4)));
        assert_eq!(r.type_, MutationType::ChildList);
        assert_eq!(r.type_.as_str(), "childList");
        assert_eq!(r.target, n(1));
        assert_eq!(r.added_nodes, vec![n(2)]);
        assert_eq!(r.removed_nodes, vec![n(3)]);
        assert_eq!(r.previous_sibling, Some(n(0)));
        assert_eq!(r.next_sibling, Some(n(4)));
        assert!(r.attribute_name.is_none());
    }

    #[test]
    fn attribute_record_carries_name_and_old_value() {
        let r = MutationRecord::attribute(n(1), "class", None, Some("old".into()));
        assert_eq!(r.type_, MutationType::Attributes);
        assert_eq!(r.attribute_name.as_deref(), Some("class"));
        assert_eq!(r.old_value.as_deref(), Some("old"));
    }

    #[test]
    fn character_data_record_carries_old_value() {
        let r = MutationRecord::character_data(n(1), Some("before".into()));
        assert_eq!(r.type_, MutationType::CharacterData);
        assert_eq!(r.old_value.as_deref(), Some("before"));
    }

    // --- MutationObserverInit validity -------------------------------

    #[test]
    fn init_requires_at_least_one_observation() {
        assert!(!MutationObserverInit::default().is_valid());
        let o = MutationObserverInit {
            child_list: true,
            ..Default::default()
        };
        assert!(o.is_valid());
    }

    #[test]
    fn attribute_filter_restricts() {
        let o = MutationObserverInit {
            attributes: true,
            attribute_filter: Some(vec!["class".into(), "id".into()]),
            ..Default::default()
        };
        assert!(o.allows_attribute("class"));
        assert!(o.allows_attribute("id"));
        assert!(!o.allows_attribute("hidden"));
    }

    #[test]
    fn no_attribute_filter_allows_all() {
        let o = MutationObserverInit {
            attributes: true,
            ..Default::default()
        };
        assert!(o.allows_attribute("anything"));
    }

    // --- should_observe ----------------------------------------------

    #[test]
    fn child_list_target_observed_when_enabled() {
        let o = MutationObserverInit {
            child_list: true,
            ..Default::default()
        };
        assert!(should_observe(
            Relation::Target,
            MutationType::ChildList,
            None,
            &o
        ));
    }

    #[test]
    fn child_list_descendant_requires_subtree() {
        let o = MutationObserverInit {
            child_list: true,
            ..Default::default()
        };
        assert!(!should_observe(
            Relation::Descendant,
            MutationType::ChildList,
            None,
            &o
        ));
        let o = MutationObserverInit {
            child_list: true,
            subtree: true,
            ..Default::default()
        };
        assert!(should_observe(
            Relation::Descendant,
            MutationType::ChildList,
            None,
            &o
        ));
    }

    #[test]
    fn attributes_filtered_by_name() {
        let o = MutationObserverInit {
            attributes: true,
            attribute_filter: Some(vec!["class".into()]),
            ..Default::default()
        };
        assert!(should_observe(
            Relation::Target,
            MutationType::Attributes,
            Some("class"),
            &o
        ));
        assert!(!should_observe(
            Relation::Target,
            MutationType::Attributes,
            Some("id"),
            &o
        ));
    }

    #[test]
    fn disabled_type_not_observed() {
        let o = MutationObserverInit {
            child_list: true,
            ..Default::default()
        };
        assert!(!should_observe(
            Relation::Target,
            MutationType::Attributes,
            None,
            &o
        ));
        assert!(!should_observe(
            Relation::Target,
            MutationType::CharacterData,
            None,
            &o
        ));
    }

    // --- MutationObserver surface ------------------------------------

    #[test]
    fn observe_rejects_invalid_options() {
        let mut mo = MutationObserver::new();
        assert!(!mo.observe(n(1), MutationObserverInit::default()));
        assert!(mo.registrations().is_empty());
    }

    #[test]
    fn observe_replaces_existing_for_same_target() {
        let mut mo = MutationObserver::new();
        let o1 = MutationObserverInit {
            child_list: true,
            ..Default::default()
        };
        mo.observe(n(1), o1);
        let o2 = MutationObserverInit {
            attributes: true,
            ..Default::default()
        };
        mo.observe(n(1), o2);
        assert_eq!(
            mo.registrations().len(),
            1,
            "re-observe replaces, not appends"
        );
        assert!(mo.registrations()[0].1.attributes);
        assert!(!mo.registrations()[0].1.child_list);
    }

    #[test]
    fn disconnect_clears_registrations_only() {
        let mut mo = MutationObserver::new();
        let o = MutationObserverInit {
            child_list: true,
            ..Default::default()
        };
        mo.observe(n(1), o);
        mo.enqueue(MutationRecord::child_list(n(1), vec![], vec![], None, None));
        mo.disconnect();
        assert!(mo.registrations().is_empty());
        assert_eq!(mo.pending_count(), 1, "pending records survive disconnect");
    }

    #[test]
    fn take_records_drains_and_clears() {
        let mut mo = MutationObserver::new();
        mo.enqueue(MutationRecord::child_list(
            n(1),
            vec![n(2)],
            vec![],
            None,
            None,
        ));
        mo.enqueue(MutationRecord::child_list(
            n(1),
            vec![],
            vec![n(3)],
            None,
            None,
        ));
        let drained = mo.take_records();
        assert_eq!(drained.len(), 2);
        assert_eq!(mo.pending_count(), 0);
        // A second drain is empty.
        assert!(mo.take_records().is_empty());
    }

    #[test]
    fn drain_for_delivery_batches_all_pending() {
        let mut mo = MutationObserver::new();
        for i in 0..5 {
            mo.enqueue(MutationRecord::child_list(
                n(1),
                vec![n(i)],
                vec![],
                None,
                None,
            ));
        }
        let batch = mo.drain_for_delivery();
        assert_eq!(
            batch.len(),
            5,
            "the microtask delivery batches every pending record"
        );
        assert_eq!(mo.pending_count(), 0);
    }
}
