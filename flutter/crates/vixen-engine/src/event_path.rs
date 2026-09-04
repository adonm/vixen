//! Composed event dispatch — pure logic for the two invariants pinned in
//! docs/SPEC.md "Composed event dispatch invariants":
//!
//! 1. **`composedPath()`** walks the target → parentNode chain, respecting
//!    shadow-DOM boundaries based on the event's `composed` flag.
//! 2. **Focus transition ordering** is exactly `focusout → focusin → blur →
//!    focus`, where `focusout`/`focusin` bubble and `blur`/`focus` do not.
//!
//! The full DOM event-dispatch machinery (capture/target/bubble retargeting,
//! listener invocation) lives in the JS runtime host-hook layer (Phase 6).
//! What lives here is the *ordering* and *path-shape* logic, which is pure
//! over a parent-pointer tree and therefore Rust-unit-tested (docs/PLAN.md
//! "Testing strategy": Rust tests cover pure logic).
//!
//! Reference: WHATWG DOM § "dispatching events" / "focusing steps".

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// composedPath()
// ---------------------------------------------------------------------------

/// A node in the path-walking model: an index into the caller's arena plus a
/// parent pointer and whether it is a shadow root (the boundary that
/// `composed: false` clamps at).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathNode {
    pub id: usize,
    pub parent: Option<usize>,
    pub is_shadow_root: bool,
}

/// Build `composedPath()`. `target` is the node id the event is dispatched
/// at; `nodes` maps id → [`PathNode`]. Returns the flat id array in path
/// order (target first).
///
/// - `composed: true` → walk through every shadow boundary to the document.
/// - `composed: false` → stop after the root of the target's own tree: the
///   shadow root is the last entry, the host (and beyond) is not included.
pub fn composed_path(target: usize, nodes: &[PathNode], composed: bool) -> Vec<usize> {
    // find a node by id
    let lookup = |id: usize| -> Option<PathNode> { nodes.iter().copied().find(|n| n.id == id) };

    let mut path = vec![target];
    let mut cur = target;
    while let Some(node) = lookup(cur) {
        let Some(parent_id) = node.parent else { break };
        path.push(parent_id);
        // composed=false clamps at the shadow root: include it, then stop
        // (do not cross into the host).
        if !composed
            && let Some(parent) = lookup(parent_id)
            && parent.is_shadow_root
        {
            break;
        }
        cur = parent_id;
    }
    path
}

// ---------------------------------------------------------------------------
// Focus transition ordering
// ---------------------------------------------------------------------------

/// One dispatched focus event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusDispatch {
    /// `"focusout"` / `"focusin"` / `"blur"` / `"focus"`.
    pub event: &'static str,
    /// Element id the event is dispatched on (`None` if no element on that side).
    pub target: Option<usize>,
    /// `focusout` and `focusin` bubble; `blur` and `focus` do not (SPEC).
    pub bubbles: bool,
}

/// Produce the focus transition event sequence per docs/SPEC.md:
///
/// ```text
/// focusout → focusin → blur → focus
/// ```
///
/// `old`/`new` are the previously- and newly-focused element ids. If `old`
/// is `None` (no prior focus), `focusout`/`blur` are omitted; if `new` is
/// `None` (document is losing focus), `focusin`/`focus` are omitted. When
/// `old == new` (no transition) the sequence is empty — no spurious events.
pub fn focus_event_sequence(old: Option<usize>, new: Option<usize>) -> Vec<FocusDispatch> {
    if old == new {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(4);
    if let Some(o) = old {
        out.push(FocusDispatch {
            event: "focusout",
            target: Some(o),
            bubbles: true,
        });
    }
    if let Some(n) = new {
        out.push(FocusDispatch {
            event: "focusin",
            target: Some(n),
            bubbles: true,
        });
    }
    if let Some(o) = old {
        out.push(FocusDispatch {
            event: "blur",
            target: Some(o),
            bubbles: false,
        });
    }
    if let Some(n) = new {
        out.push(FocusDispatch {
            event: "focus",
            target: Some(n),
            bubbles: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tree:
    //   0 (document)
    //   └ 1 (host <div>)
    //     └ 2 (shadow root)        ← boundary
    //       └ 3 (slot host)
    //         └ 4 (target)
    fn shadow_tree() -> Vec<PathNode> {
        vec![
            PathNode {
                id: 0,
                parent: None,
                is_shadow_root: false,
            },
            PathNode {
                id: 1,
                parent: Some(0),
                is_shadow_root: false,
            },
            PathNode {
                id: 2,
                parent: Some(1),
                is_shadow_root: true,
            }, // shadow root
            PathNode {
                id: 3,
                parent: Some(2),
                is_shadow_root: false,
            },
            PathNode {
                id: 4,
                parent: Some(3),
                is_shadow_root: false,
            },
        ]
    }

    // --- composedPath --------------------------------------------------

    #[test]
    fn composed_true_crosses_shadow_boundary() {
        let path = composed_path(4, &shadow_tree(), true);
        // 4 → 3 → 2(shadowRoot) → 1(host) → 0(document)
        assert_eq!(path, vec![4, 3, 2, 1, 0]);
    }

    #[test]
    fn composed_false_clamps_at_shadow_root() {
        let path = composed_path(4, &shadow_tree(), false);
        // Target's tree root is the shadow root (id 2); include it, stop.
        assert_eq!(path, vec![4, 3, 2]);
    }

    #[test]
    fn composed_path_in_light_tree_is_unaffected_by_flag() {
        // A target in the main document never crosses a shadow boundary.
        let light = vec![
            PathNode {
                id: 0,
                parent: None,
                is_shadow_root: false,
            },
            PathNode {
                id: 1,
                parent: Some(0),
                is_shadow_root: false,
            },
            PathNode {
                id: 2,
                parent: Some(1),
                is_shadow_root: false,
            },
        ];
        assert_eq!(composed_path(2, &light, false), vec![2, 1, 0]);
        assert_eq!(composed_path(2, &light, true), vec![2, 1, 0]);
    }

    #[test]
    fn composed_path_root_only() {
        let single = vec![PathNode {
            id: 7,
            parent: None,
            is_shadow_root: false,
        }];
        assert_eq!(composed_path(7, &single, true), vec![7]);
    }

    // --- focus ordering -----------------------------------------------

    #[test]
    fn focus_full_transition_order_and_bubbling() {
        let seq = focus_event_sequence(Some(1), Some(2));
        assert_eq!(
            seq.iter().map(|f| f.event).collect::<Vec<_>>(),
            vec!["focusout", "focusin", "blur", "focus"]
        );
        // focusout/focusin bubble; blur/focus do not (SPEC).
        let bubbles: Vec<bool> = seq.iter().map(|f| f.bubbles).collect();
        assert_eq!(bubbles, vec![true, true, false, false]);
        // Targets: focusout/blur on old; focusin/focus on new.
        assert_eq!(
            seq.iter().map(|f| f.target).collect::<Vec<_>>(),
            vec![Some(1), Some(2), Some(1), Some(2)]
        );
    }

    #[test]
    fn focus_gain_from_none_omits_out_and_blur() {
        let seq = focus_event_sequence(None, Some(5));
        assert_eq!(
            seq.iter().map(|f| f.event).collect::<Vec<_>>(),
            vec!["focusin", "focus"]
        );
    }

    #[test]
    fn focus_loss_to_none_omits_in_and_focus() {
        let seq = focus_event_sequence(Some(5), None);
        assert_eq!(
            seq.iter().map(|f| f.event).collect::<Vec<_>>(),
            vec!["focusout", "blur"]
        );
    }

    #[test]
    fn focus_no_change_emits_nothing() {
        assert!(focus_event_sequence(Some(3), Some(3)).is_empty());
        assert!(focus_event_sequence(None, None).is_empty());
    }
}
