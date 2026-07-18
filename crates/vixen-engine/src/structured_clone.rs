//! HTML § 2.7.5 — the structured clone algorithm (pure logic). The
//! serialization `postMessage()`, `history.pushState()`, IndexedDB, and the
//! `MessageChannel` / `MessagePort` surface reduce to. Pairs with
//! [`crate::message_port`] (which consumes [`clone`] to move a value across a
//! port pair) and the cross-origin-isolation gate ([`crate::message_port`]
//! consults when transferring `SharedArrayBuffer`).
//!
//! What lives here:
//! - [`StructuredCloneValue`] — the type-tagged tree of serializable values
//!   the spec's "structured clone internal method" walks. Carries the
//!   primitive family, the container family (records, lists, maps, sets), the
//!   typed-array family (as [`Buffer`] handles the host hook resolves to real
//!   `ArrayBuffer`s), the temporal family (`Date`/`Duration`/timestamp), the
//!   error family, and the transferable handle family (`MessagePort` /
//!   `ArrayBuffer` / `SharedArrayBuffer`).
//! - [`DataCloneError`] — the `DOMException`-named rejections the spec raises
//!   (a non-cloneable type, a duplicate transferable, a detached buffer, a
//!   transferred-but-unreachable port).
//! - [`clone`] — the deep-clone the algorithm performs, honouring the
//!   transfer list (transferred `MessagePort`s detach; transferred
//!   `ArrayBuffer`s detach; transferred-but-not-present handles are a
//!   `DataCloneError`).
//! - [`is_cloneable`] — the partial-check predicate a host hook calls before
//!   walking (used to surface a `DataCloneError` *before* any side-effectful
//!   transfer detachment).
//!
//! What does *not* live here:
//! - Shared-reference identity preservation (the spec's "memory" map: two
//!   pointers to the same object clone to the same object, not two copies).
//!   A pure Rust tree has no object identity; the host hook layer (which has
//!   real JS object identities) owns the memory map. The clone here is a
//!   faithful tree-clone for tree inputs.
//! - The wire serialization (`SerializeToArrayBuffer` / the QV1 binary wire
//!   format). The host hook layer owns the on-disk / over-the-wire bytes;
//!   this module is the in-memory algorithm.
//! - Platform objects (`DOMException`, `File`, `Blob`, `ImageData`,
//!   `DOMMatrix`, …). Each lands with its Phase 6 host hook; the
//!   [`StructuredCloneValue::PlatformObject`] variant reserves the slot.
//!
//! Reference: <https://html.spec.whatwg.org/multipage/structured-data.html#structuredclone>.

#![forbid(unsafe_code)]

use crate::message_port::PortId;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Transferable handle
// ---------------------------------------------------------------------------

/// A typed-array buffer handle. The host hook owns the real `ArrayBuffer`
/// storage; the structured-clone algorithm only needs to track identity (for
/// transfer detachment) and whether the buffer is shareable (the
/// cross-origin-isolation gate consults [`Buffer::shared`] before transferring
/// a `SharedArrayBuffer`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Buffer {
    /// Stable identity used for transfer-list dedup.
    pub id: u64,
    /// `true` for `SharedArrayBuffer` (memory is shared, not transferred).
    pub shared: bool,
    /// `true` once a transfer has detached this buffer (any further use is a
    /// `DataCloneError` per HTML § 2.7.5 step 5).
    pub detached: bool,
}

/// A transferable handle. HTML § 2.7.5 step 4: the transfer list is a list of
/// "transferable objects" — `MessagePort`s, `ArrayBuffer`s /
/// `SharedArrayBuffer`s, and (in newer revisions) `ReadableStream` /
/// `WritableStream` / `TransformStream` / `AudioData` / `VideoFrame` &c.
/// Modelled here as the v1.0 subset.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Transferable {
    /// A `MessagePort`. Transferring detaches the source port and re-entangles
    /// the destination port ([`crate::message_port::MessagePort::transfer`]).
    MessagePort(PortId),
    /// An `ArrayBuffer` (transfer detaches the source).
    ArrayBuffer(u64),
    /// A `SharedArrayBuffer`. Transfer is *only* permitted in a cross-origin-
    /// isolated context (HTML § 7.2); the gate is [`crate::message_port`] /
    /// the embedder's job to consult before calling [`clone`].
    SharedArrayBuffer(u64),
}

// ---------------------------------------------------------------------------
// StructuredCloneValue
// ---------------------------------------------------------------------------

/// The type-tagged tree of serializable values the structured clone algorithm
/// walks (HTML § 2.7.5 "structured clone internal method"). Each variant is a
/// "serializable type" or "cloneable type" from the spec's tables; the
/// platform-object slot is reserved for the Phase 6 host-hook platform types.
#[derive(Debug, Clone, PartialEq)]
pub enum StructuredCloneValue {
    /// `undefined`.
    Undefined,
    /// `null`.
    Null,
    /// `boolean`.
    Boolean(bool),
    /// ECMAScript `Number` (IEEE-754 double; `NaN` round-trips, matching the
    /// spec — `Number.isNaN` is preserved).
    Number(f64),
    /// ECMAScript `BigInt`. Carried as a sign + magnitude string so the full
    /// arbitrary-precision range survives the round trip; the host hook boxes
    /// a real `BigInt` on either end.
    BigInt(String),
    /// ECMAScript `String`.
    String(String),
    /// `Date` — the § 2.7.5 time value (ms since the Unix epoch). `NaN`
    /// encodes the "invalid date" the spec preserves.
    Date(f64),
    /// `Array` — an ordered, dense list (holes are modelled as
    /// [`StructuredCloneValue::Undefined`]).
    Array(Vec<StructuredCloneValue>),
    /// `Object` — a string-keyed record, insertion-ordered (the spec preserves
    /// key insertion order; the host hook reads this back via
    /// `Object.keys`-order).
    Object(Vec<(String, StructuredCloneValue)>),
    /// `Map` — keyed by a [`StructuredCloneValue`] (any cloneable type can be
    /// a key, per spec). Insertion-ordered; the SameValueZero key-equality
    /// the spec mandates is the host hook's job when re-hydrating.
    Map(Vec<(StructuredCloneValue, StructuredCloneValue)>),
    /// `Set` — insertion-ordered unique values.
    Set(Vec<StructuredCloneValue>),
    /// `ArrayBuffer` / typed-array storage handle. [`Buffer::shared`]
    /// distinguishes `SharedArrayBuffer`.
    ArrayBuffer(Buffer),
    /// A `MessagePort` handle (the [`PortId`] resolves at the host hook).
    MessagePort(PortId),
    /// `Error` — the § 2.7.5 "Error" sub-algorithm: a kind + message + stack
    /// string. The kind selects the `Error` subclass
    /// (`Error`/`TypeError`/`RangeError`/…) the host hook rehydrates.
    Error {
        kind: ErrorKind,
        message: String,
        stack: String,
    },
    /// A platform object (`File`, `Blob`, `ImageData`, `DOMException`, …).
    /// Reserved for the Phase 6 host-hook types; the structured clone of any
    /// not-yet-landed platform type is a [`DataCloneError`] today.
    PlatformObject(String),
}

/// The `Error` subclass kind the structured-clone "Error" sub-algorithm
/// rehydrates (HTML § 2.7.5 "Error" table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ErrorKind {
    /// `Error`.
    #[default]
    Error,
    /// `EvalError`.
    EvalError,
    /// `RangeError`.
    RangeError,
    /// `ReferenceError`.
    ReferenceError,
    /// `SyntaxError`.
    SyntaxError,
    /// `TypeError`.
    TypeError,
    /// `URIError`.
    UriError,
}

impl ErrorKind {
    /// The ECMAScript constructor name the host hook rehydrates as.
    pub const fn name(self) -> &'static str {
        match self {
            ErrorKind::Error => "Error",
            ErrorKind::EvalError => "EvalError",
            ErrorKind::RangeError => "RangeError",
            ErrorKind::ReferenceError => "ReferenceError",
            ErrorKind::SyntaxError => "SyntaxError",
            ErrorKind::TypeError => "TypeError",
            ErrorKind::UriError => "URIError",
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// The `DOMException`-named rejections the structured clone algorithm raises
/// (HTML § 2.7.5). Every variant maps to a `DataCloneError` `DOMException`
/// with the given message; the host hook surfaces them as
/// `DataCloneError`-named exceptions per spec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataCloneError {
    /// The value (or a nested value) is not a serializable / cloneable type.
    /// The canonical case: a function symbol, a DOM node, a WeakRef.
    #[error("the object could not be cloned (non-cloneable {0})")]
    NotCloneable(&'static str),
    /// A platform type landed in [`StructuredCloneValue::PlatformObject`] that
    /// no host hook has registered yet.
    #[error("the {0} platform object is not yet structured-cloneable")]
    UnsupportedPlatformObject(&'static str),
    /// A transferable in the transfer list appears more than once (§ 2.7.5
    /// step 4: "If `transferList` has duplicate objects, then throw a
    /// 'DataCloneError' DOMException").
    #[error("duplicate transferable in the transfer list")]
    DuplicateTransferable,
    /// A transferable in the transfer list is detached (§ 2.7.5 step 5).
    #[error("a transferable buffer is already detached")]
    DetachedTransferable,
    /// A transferable in the transfer list was not found in the value (§ 2.7.5
    /// step 4.1: "If … not in `value`, throw"). The host hook historically
    /// tolerated this; the spec mandates rejection.
    #[error("a transferable handle in the transfer list was not in the value")]
    UnreachableTransferable,
    /// A `SharedArrayBuffer` is being cloned (not transferred) outside a
    /// cross-origin-isolated context (HTML § 7.2). The cross-origin-isolation
    /// gate is [`crate::message_port`] / the embedder's job to consult before
    /// calling [`clone`]; this is the fail-closed backstop.
    #[error("SharedArrayBuffer clone requires a cross-origin-isolated context")]
    SharedBufferRequiresIsolation,
}

// ---------------------------------------------------------------------------
// is_cloneable
// ---------------------------------------------------------------------------

/// `true` if `value` contains only serializable / cloneable types. A
/// `PlatformObject(name)` counts as cloneable only if the host hook has
/// registered that type (passed in via `known_platform_types`); the v1.0
/// default registers none, so any [`StructuredCloneValue::PlatformObject`] is
/// a [`DataCloneError::UnsupportedPlatformObject`].
///
/// This is the partial-check the host hook calls *before* walking (so a
/// `DataCloneError` surfaces before any transfer detachment side-effect).
pub fn is_cloneable(value: &StructuredCloneValue, known_platform_types: &HashSet<String>) -> bool {
    match value {
        StructuredCloneValue::Undefined
        | StructuredCloneValue::Null
        | StructuredCloneValue::Boolean(_)
        | StructuredCloneValue::Number(_)
        | StructuredCloneValue::BigInt(_)
        | StructuredCloneValue::String(_)
        | StructuredCloneValue::Date(_) => true,
        StructuredCloneValue::Array(items) => {
            items.iter().all(|v| is_cloneable(v, known_platform_types))
        }
        StructuredCloneValue::Object(entries) => entries
            .iter()
            .all(|(_, v)| is_cloneable(v, known_platform_types)),
        StructuredCloneValue::Map(entries) => entries.iter().all(|(k, v)| {
            is_cloneable(k, known_platform_types) && is_cloneable(v, known_platform_types)
        }),
        StructuredCloneValue::Set(items) => {
            items.iter().all(|v| is_cloneable(v, known_platform_types))
        }
        StructuredCloneValue::ArrayBuffer(_) | StructuredCloneValue::MessagePort(_) => true,
        StructuredCloneValue::Error { .. } => true,
        StructuredCloneValue::PlatformObject(name) => known_platform_types.contains(name),
    }
}

// ---------------------------------------------------------------------------
// clone
// ---------------------------------------------------------------------------

/// Perform the HTML § 2.7.5 structured clone of `value`, honouring the
/// transfer list. On success, the returned tree is a deep copy of `value`
/// with every transferred handle detached in the source (the caller owns the
/// detachment side-effects; this function flips [`Buffer::detached`] in place
/// and the [`crate::message_port::MessagePort`] transfer is the caller's
/// responsibility).
///
/// ### Transfer semantics
/// - Each entry in `transfer` must appear in `value`; an unreachable handle
///   is a [`DataCloneError::UnreachableTransferable`].
/// - Duplicate entries are a [`DataCloneError::DuplicateTransferable`].
/// - A detached buffer in the transfer list (or in `value`) is a
///   [`DataCloneError::DetachedTransferable`].
/// - `SharedArrayBuffer` is shareable (the buffer is not detached on
///   transfer); `ArrayBuffer` detaches the source on transfer.
/// - `MessagePort` transfer is recorded in the result; the
///   [`crate::message_port`] layer performs the entanglement hand-off.
///
/// ### Cycles
/// A pure [`StructuredCloneValue`] tree is acyclic by construction (no `Rc`
/// cycles), so the spec's "memory" map (which preserves shared-reference
/// identity) is a no-op here; the host hook owns the memory map when it has
/// real JS object identities.
pub fn clone(
    value: &StructuredCloneValue,
    transfer: &[Transferable],
    cross_origin_isolated: bool,
    known_platform_types: &HashSet<String>,
) -> Result<StructuredCloneValue, DataCloneError> {
    // § 2.7.5 step 4: validate the transfer list first.
    validate_transfer_list(transfer)?;

    // Step 4.1: every transferable must be reachable in the value.
    for handle in transfer {
        if !transfer_reachable(value, handle) {
            return Err(DataCloneError::UnreachableTransferable);
        }
    }

    // SharedArrayBuffer requires cross-origin isolation (HTML § 7.2). A
    // non-transferred SharedArrayBuffer clone copies the storage only if the
    // context is isolated; fail closed otherwise.
    if !cross_origin_isolated && contains_shared_buffer(value, transfer) {
        return Err(DataCloneError::SharedBufferRequiresIsolation);
    }

    // Step 5: walk. Transferred buffers are noted for the caller's detach
    // step (transferred ports are re-entangled at the receiver by the
    // message_port layer; the clone here only copies the value tree).
    let cloned = clone_inner(value, known_platform_types)?;

    Ok(cloned)
}

fn clone_inner(
    value: &StructuredCloneValue,
    known_platform_types: &HashSet<String>,
) -> Result<StructuredCloneValue, DataCloneError> {
    match value {
        StructuredCloneValue::Undefined
        | StructuredCloneValue::Null
        | StructuredCloneValue::Boolean(_)
        | StructuredCloneValue::Number(_)
        | StructuredCloneValue::BigInt(_)
        | StructuredCloneValue::String(_)
        | StructuredCloneValue::Date(_) => Ok(value.clone()),
        StructuredCloneValue::Array(items) => {
            let out = items
                .iter()
                .map(|v| clone_inner(v, known_platform_types))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(StructuredCloneValue::Array(out))
        }
        StructuredCloneValue::Object(entries) => {
            let out = entries
                .iter()
                .map(|(k, v)| Ok((k.clone(), clone_inner(v, known_platform_types)?)))
                .collect::<Result<Vec<_>, DataCloneError>>()?;
            Ok(StructuredCloneValue::Object(out))
        }
        StructuredCloneValue::Map(entries) => {
            let out = entries
                .iter()
                .map(|(k, v)| {
                    Ok((
                        clone_inner(k, known_platform_types)?,
                        clone_inner(v, known_platform_types)?,
                    ))
                })
                .collect::<Result<Vec<_>, DataCloneError>>()?;
            Ok(StructuredCloneValue::Map(out))
        }
        StructuredCloneValue::Set(items) => {
            let out = items
                .iter()
                .map(|v| clone_inner(v, known_platform_types))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(StructuredCloneValue::Set(out))
        }
        StructuredCloneValue::ArrayBuffer(buf) => {
            if buf.detached {
                return Err(DataCloneError::DetachedTransferable);
            }
            // Transferred ArrayBuffers detach the source; SharedArrayBuffers
            // stay shared. In both cases the *cloned* value references the
            // same handle id (the host hook hands the storage over).
            Ok(StructuredCloneValue::ArrayBuffer(buf.clone()))
        }
        StructuredCloneValue::MessagePort(id) => {
            // The cloned value references the same port id; the host hook
            // (and the message_port layer) performs the entanglement
            // hand-off if this port is in the transfer list.
            Ok(StructuredCloneValue::MessagePort(*id))
        }
        StructuredCloneValue::Error {
            kind,
            message,
            stack,
        } => Ok(StructuredCloneValue::Error {
            kind: *kind,
            message: message.clone(),
            stack: stack.clone(),
        }),
        StructuredCloneValue::PlatformObject(name) => {
            if known_platform_types.contains(name) {
                Ok(StructuredCloneValue::PlatformObject(name.clone()))
            } else {
                Err(DataCloneError::UnsupportedPlatformObject(leak_name(name)))
            }
        }
    }
}

/// Detach the source-tree buffers that the transfer list handed over. Called
/// by the host hook after [`clone`] succeeds; kept separate so [`clone`]
/// stays pure w.r.t. its `value` argument. `ArrayBuffer`s detach;
/// `SharedArrayBuffer`s do not (their storage is shared).
pub fn detach_transferred(value: &mut StructuredCloneValue, transfer: &[Transferable]) {
    let mut to_detach: HashSet<u64> = HashSet::new();
    for handle in transfer {
        if let Transferable::ArrayBuffer(id) = handle {
            to_detach.insert(*id);
        }
    }
    detach_inner(value, &to_detach);
}

fn detach_inner(value: &mut StructuredCloneValue, to_detach: &HashSet<u64>) {
    match value {
        StructuredCloneValue::ArrayBuffer(buf) if to_detach.contains(&buf.id) => {
            buf.detached = true;
        }
        StructuredCloneValue::Array(items) => {
            for v in items {
                detach_inner(v, to_detach);
            }
        }
        StructuredCloneValue::Object(entries) => {
            for (_, v) in entries {
                detach_inner(v, to_detach);
            }
        }
        StructuredCloneValue::Map(entries) => {
            for (k, v) in entries {
                detach_inner(k, to_detach);
                detach_inner(v, to_detach);
            }
        }
        StructuredCloneValue::Set(items) => {
            for v in items {
                detach_inner(v, to_detach);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Transfer validation
// ---------------------------------------------------------------------------

fn validate_transfer_list(transfer: &[Transferable]) -> Result<(), DataCloneError> {
    let mut seen: HashSet<&Transferable> = HashSet::new();
    for handle in transfer {
        if !seen.insert(handle) {
            return Err(DataCloneError::DuplicateTransferable);
        }
    }
    Ok(())
}

fn transfer_reachable(value: &StructuredCloneValue, handle: &Transferable) -> bool {
    match handle {
        Transferable::MessagePort(id) => contains_port(value, *id),
        Transferable::ArrayBuffer(id) => contains_buffer(value, *id, false),
        Transferable::SharedArrayBuffer(id) => contains_buffer(value, *id, true),
    }
}

fn contains_port(value: &StructuredCloneValue, id: PortId) -> bool {
    match value {
        StructuredCloneValue::MessagePort(p) => *p == id,
        StructuredCloneValue::Array(items) => items.iter().any(|v| contains_port(v, id)),
        StructuredCloneValue::Object(entries) => entries.iter().any(|(_, v)| contains_port(v, id)),
        StructuredCloneValue::Map(entries) => entries
            .iter()
            .any(|(k, v)| contains_port(k, id) || contains_port(v, id)),
        StructuredCloneValue::Set(items) => items.iter().any(|v| contains_port(v, id)),
        _ => false,
    }
}

fn contains_buffer(value: &StructuredCloneValue, id: u64, shared: bool) -> bool {
    match value {
        StructuredCloneValue::ArrayBuffer(buf) => buf.id == id && buf.shared == shared,
        StructuredCloneValue::Array(items) => items.iter().any(|v| contains_buffer(v, id, shared)),
        StructuredCloneValue::Object(entries) => {
            entries.iter().any(|(_, v)| contains_buffer(v, id, shared))
        }
        StructuredCloneValue::Map(entries) => entries
            .iter()
            .any(|(k, v)| contains_buffer(k, id, shared) || contains_buffer(v, id, shared)),
        StructuredCloneValue::Set(items) => items.iter().any(|v| contains_buffer(v, id, shared)),
        _ => false,
    }
}

fn contains_shared_buffer(value: &StructuredCloneValue, transfer: &[Transferable]) -> bool {
    // A SharedArrayBuffer present in the value but NOT in the transfer list
    // triggers the cross-origin-isolation gate (a *clone*, not a transfer).
    for handle in transfer {
        if let Transferable::SharedArrayBuffer(id) = handle
            && contains_buffer(value, *id, true)
        {
            return false;
        }
    }
    contains_buffer_kind(value, true)
}

fn contains_buffer_kind(value: &StructuredCloneValue, shared: bool) -> bool {
    match value {
        StructuredCloneValue::ArrayBuffer(buf) => buf.shared == shared && !buf.detached,
        StructuredCloneValue::Array(items) => items.iter().any(|v| contains_buffer_kind(v, shared)),
        StructuredCloneValue::Object(entries) => {
            entries.iter().any(|(_, v)| contains_buffer_kind(v, shared))
        }
        StructuredCloneValue::Map(entries) => entries
            .iter()
            .any(|(k, v)| contains_buffer_kind(k, shared) || contains_buffer_kind(v, shared)),
        StructuredCloneValue::Set(items) => items.iter().any(|v| contains_buffer_kind(v, shared)),
        _ => false,
    }
}

/// Promote a borrowed `&str` name to a `&'static str` for the error variant.
/// Leaks the (small, caller-bounded) name; the host hook supplies these from
/// a static table in practice. Kept simple to stay `forbid(unsafe_code)`.
fn leak_name(name: &str) -> &'static str {
    // The UnsupportedPlatformObject variant is rare and the names are short;
    // for the v1.0 surface the host hook registers no platform types, so this
    // path is unreachable in the default config. Box::leak keeps the value
    // alive for the program's duration without `unsafe`.
    let boxed: Box<str> = Box::from(name);
    Box::leak(boxed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn no_platform_types() -> HashSet<String> {
        HashSet::new()
    }

    // --- is_cloneable --------------------------------------------------

    #[test]
    fn primitives_are_cloneable() {
        let set = no_platform_types();
        assert!(is_cloneable(&StructuredCloneValue::Undefined, &set));
        assert!(is_cloneable(&StructuredCloneValue::Null, &set));
        assert!(is_cloneable(&StructuredCloneValue::Boolean(true), &set));
        assert!(is_cloneable(&StructuredCloneValue::Number(1.5), &set));
        assert!(is_cloneable(&StructuredCloneValue::Number(f64::NAN), &set));
        assert!(is_cloneable(
            &StructuredCloneValue::String("hi".into()),
            &set
        ));
        assert!(is_cloneable(
            &StructuredCloneValue::BigInt("42".into()),
            &set
        ));
        assert!(is_cloneable(&StructuredCloneValue::Date(0.0), &set));
    }

    #[test]
    fn nested_containers_cloneable() {
        let set = no_platform_types();
        let v = StructuredCloneValue::Array(vec![
            StructuredCloneValue::Object(vec![("a".into(), StructuredCloneValue::Number(1.0))]),
            StructuredCloneValue::Set(vec![StructuredCloneValue::String("x".into())]),
        ]);
        assert!(is_cloneable(&v, &set));
    }

    #[test]
    fn unknown_platform_object_not_cloneable() {
        let set = no_platform_types();
        let v = StructuredCloneValue::PlatformObject("File".into());
        assert!(!is_cloneable(&v, &set));
    }

    #[test]
    fn registered_platform_object_cloneable() {
        let mut set = no_platform_types();
        set.insert("Blob".into());
        let v = StructuredCloneValue::PlatformObject("Blob".into());
        assert!(is_cloneable(&v, &set));
    }

    #[test]
    fn unknown_platform_object_nested_blocks_parent() {
        let set = no_platform_types();
        let v =
            StructuredCloneValue::Array(vec![StructuredCloneValue::PlatformObject("File".into())]);
        assert!(!is_cloneable(&v, &set));
    }

    // --- clone primitives ---------------------------------------------

    #[test]
    fn clone_primitives_round_trip() {
        let set = no_platform_types();
        let cases: Vec<StructuredCloneValue> = vec![
            StructuredCloneValue::Undefined,
            StructuredCloneValue::Null,
            StructuredCloneValue::Boolean(true),
            StructuredCloneValue::Number(2.5),
            StructuredCloneValue::Number(f64::INFINITY),
            StructuredCloneValue::String("hello".into()),
            StructuredCloneValue::BigInt("-9000000000000000001".into()),
            StructuredCloneValue::Date(1_700_000_000_000.0),
        ];
        for v in cases {
            let out = clone(&v, &[], true, &set).unwrap();
            assert_eq!(out, v, "round trip failed for {v:?}");
        }

        // NaN doesn't equal itself under derived PartialEq; check by bit
        // pattern so the structured clone's NaN-preservation (HTML § 2.7.5
        // keeps NaN as NaN) is asserted.
        let nan = StructuredCloneValue::Number(f64::NAN);
        let out = clone(&nan, &[], true, &set).unwrap();
        match out {
            StructuredCloneValue::Number(n) => assert!(n.is_nan()),
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[test]
    fn clone_object_preserves_key_order() {
        let set = no_platform_types();
        let v = StructuredCloneValue::Object(vec![
            ("z".into(), StructuredCloneValue::Number(1.0)),
            ("a".into(), StructuredCloneValue::Number(2.0)),
            ("m".into(), StructuredCloneValue::Number(3.0)),
        ]);
        let out = clone(&v, &[], true, &set).unwrap();
        let StructuredCloneValue::Object(entries) = out else {
            panic!("expected object");
        };
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["z", "a", "m"]);
    }

    #[test]
    fn clone_array_with_holes_as_undefined() {
        let set = no_platform_types();
        let v = StructuredCloneValue::Array(vec![
            StructuredCloneValue::Number(1.0),
            StructuredCloneValue::Undefined, // a hole
            StructuredCloneValue::Number(3.0),
        ]);
        let out = clone(&v, &[], true, &set).unwrap();
        assert_eq!(out, v);
    }

    #[test]
    fn clone_map_and_set() {
        let set = no_platform_types();
        let v = StructuredCloneValue::Map(vec![
            (
                StructuredCloneValue::String("k".into()),
                StructuredCloneValue::Number(1.0),
            ),
            (
                StructuredCloneValue::Number(2.0),
                StructuredCloneValue::Array(vec![StructuredCloneValue::Boolean(true)]),
            ),
        ]);
        let out = clone(&v, &[], true, &set).unwrap();
        assert_eq!(out, v);

        let s = StructuredCloneValue::Set(vec![
            StructuredCloneValue::String("a".into()),
            StructuredCloneValue::Null,
        ]);
        assert_eq!(clone(&s, &[], true, &set).unwrap(), s);
    }

    #[test]
    fn clone_error_preserves_kind_message_stack() {
        let set = no_platform_types();
        let v = StructuredCloneValue::Error {
            kind: ErrorKind::TypeError,
            message: "not a function".into(),
            stack: "at foo (script.js:1)\n".into(),
        };
        let out = clone(&v, &[], true, &set).unwrap();
        assert_eq!(out, v);
    }

    #[test]
    fn clone_deep_does_not_share_writes() {
        // A pure tree clone is acyclic; mutating the clone must not affect the
        // source (no shared references).
        let set = no_platform_types();
        let v = StructuredCloneValue::Array(vec![StructuredCloneValue::Number(1.0)]);
        let mut out = clone(&v, &[], true, &set).unwrap();
        if let StructuredCloneValue::Array(items) = &mut out {
            items[0] = StructuredCloneValue::Number(99.0);
        }
        // Source is unchanged.
        let StructuredCloneValue::Array(src) = &v else {
            unreachable!()
        };
        assert_eq!(src[0], StructuredCloneValue::Number(1.0));
    }

    // --- unsupported platform type ------------------------------------

    #[test]
    fn clone_unknown_platform_object_errors() {
        let set = no_platform_types();
        let v = StructuredCloneValue::PlatformObject("File".into());
        let err = clone(&v, &[], true, &set).unwrap_err();
        assert!(matches!(err, DataCloneError::UnsupportedPlatformObject(_)));
    }

    // --- ArrayBuffer transfer -----------------------------------------

    #[test]
    fn clone_array_buffer_without_transfer_copies_handle() {
        let set = no_platform_types();
        let buf = StructuredCloneValue::ArrayBuffer(Buffer {
            id: 7,
            shared: false,
            detached: false,
        });
        let out = clone(&buf, &[], true, &set).unwrap();
        assert_eq!(out, buf);
    }

    #[test]
    fn detach_transferred_array_buffer_detaches_source() {
        let mut v = StructuredCloneValue::ArrayBuffer(Buffer {
            id: 7,
            shared: false,
            detached: false,
        });
        detach_transferred(&mut v, &[Transferable::ArrayBuffer(7)]);
        let StructuredCloneValue::ArrayBuffer(buf) = &v else {
            panic!()
        };
        assert!(buf.detached);
    }

    #[test]
    fn detach_transferred_leaves_shared_buffer_intact() {
        // SharedArrayBuffer is shared, not transferred; the source is not
        // detached.
        let mut v = StructuredCloneValue::ArrayBuffer(Buffer {
            id: 9,
            shared: true,
            detached: false,
        });
        detach_transferred(&mut v, &[Transferable::SharedArrayBuffer(9)]);
        let StructuredCloneValue::ArrayBuffer(buf) = &v else {
            panic!()
        };
        assert!(!buf.detached);
    }

    #[test]
    fn clone_detached_buffer_errors() {
        let set = no_platform_types();
        let v = StructuredCloneValue::ArrayBuffer(Buffer {
            id: 1,
            shared: false,
            detached: true,
        });
        let err = clone(&v, &[], true, &set).unwrap_err();
        assert_eq!(err, DataCloneError::DetachedTransferable);
    }

    // --- transfer list validation -------------------------------------

    #[test]
    fn duplicate_transferable_errors() {
        let set = no_platform_types();
        let v = StructuredCloneValue::MessagePort(PortId(1));
        let transfer = vec![
            Transferable::MessagePort(PortId(1)),
            Transferable::MessagePort(PortId(1)),
        ];
        let err = clone(&v, &transfer, true, &set).unwrap_err();
        assert_eq!(err, DataCloneError::DuplicateTransferable);
    }

    #[test]
    fn unreachable_transferable_errors() {
        let set = no_platform_types();
        let v = StructuredCloneValue::MessagePort(PortId(1));
        // Port 2 is not in the value.
        let err = clone(&v, &[Transferable::MessagePort(PortId(2))], true, &set).unwrap_err();
        assert_eq!(err, DataCloneError::UnreachableTransferable);
    }

    #[test]
    fn unreachable_array_buffer_in_nested_tree_errors() {
        let set = no_platform_types();
        let v = StructuredCloneValue::Array(vec![StructuredCloneValue::ArrayBuffer(Buffer {
            id: 5,
            shared: false,
            detached: false,
        })]);
        // Buffer 99 not in the value.
        let err = clone(&v, &[Transferable::ArrayBuffer(99)], true, &set).unwrap_err();
        assert_eq!(err, DataCloneError::UnreachableTransferable);
    }

    #[test]
    fn reachable_array_buffer_in_nested_tree_succeeds() {
        let set = no_platform_types();
        let v = StructuredCloneValue::Array(vec![StructuredCloneValue::ArrayBuffer(Buffer {
            id: 5,
            shared: false,
            detached: false,
        })]);
        let out = clone(&v, &[Transferable::ArrayBuffer(5)], true, &set).unwrap();
        assert_eq!(out, v);
    }

    // --- SharedArrayBuffer isolation gate ------------------------------

    #[test]
    fn shared_buffer_clone_fails_without_isolation() {
        let set = no_platform_types();
        let v = StructuredCloneValue::ArrayBuffer(Buffer {
            id: 3,
            shared: true,
            detached: false,
        });
        let err = clone(&v, &[], false, &set).unwrap_err();
        assert_eq!(err, DataCloneError::SharedBufferRequiresIsolation);
    }

    #[test]
    fn shared_buffer_clone_succeeds_with_isolation() {
        let set = no_platform_types();
        let v = StructuredCloneValue::ArrayBuffer(Buffer {
            id: 3,
            shared: true,
            detached: false,
        });
        let out = clone(&v, &[], true, &set).unwrap();
        assert_eq!(out, v);
    }

    #[test]
    fn shared_buffer_transfer_bypasses_isolation_gate() {
        // Transferring a SharedArrayBuffer is permitted regardless of
        // isolation (the storage is shared, not copied); the isolation gate
        // is only about *cloning* (copying the storage).
        let set = no_platform_types();
        let v = StructuredCloneValue::ArrayBuffer(Buffer {
            id: 3,
            shared: true,
            detached: false,
        });
        let out = clone(&v, &[Transferable::SharedArrayBuffer(3)], false, &set).unwrap();
        assert_eq!(out, v);
    }

    // --- ErrorKind names ----------------------------------------------

    #[test]
    fn error_kind_names() {
        assert_eq!(ErrorKind::Error.name(), "Error");
        assert_eq!(ErrorKind::RangeError.name(), "RangeError");
        assert_eq!(ErrorKind::TypeError.name(), "TypeError");
        assert_eq!(ErrorKind::UriError.name(), "URIError");
    }

    // --- Doctests-style sanity ----------------------------------------

    #[test]
    fn clone_no_transfer_is_deep_copy() {
        let set = no_platform_types();
        let v = StructuredCloneValue::Object(vec![
            ("a".into(), StructuredCloneValue::Number(1.0)),
            (
                "b".into(),
                StructuredCloneValue::Array(vec![StructuredCloneValue::Boolean(false)]),
            ),
        ]);
        let out = clone(&v, &[], true, &set).unwrap();
        assert_eq!(out, v);
    }
}
