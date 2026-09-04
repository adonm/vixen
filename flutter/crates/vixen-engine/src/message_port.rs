//! HTML § 9.5 — `MessagePort` / `MessageChannel` (pure logic). The entangled
//! port-pair + message-queue model `window.postMessage()`,
//! `new MessageChannel()`, `new MessagePort()`, and the worker
//! `postMessage()` surface reduce to. The structured-clone over the wire is
//! [`crate::structured_clone`]; the actual event-loop delivery (firing
//! `onmessage`) is the host hook's job.
//!
//! What lives here:
//! - [`PortId`] — the opaque handle used in
//!   [`crate::structured_clone::StructuredCloneValue::MessagePort`] and the
//!   transfer list.
//! - [`MessagePort`] — one end of an entangled pair. Carries the message
//!   inbox (a queue of cloned values + transferred-port handles) + the
//!   `detached` flag + the entangled-partner id.
//! - [`MessageChannel`] — owns the two ports created by
//!   `new MessageChannel()`; [`MessageChannel::new`] is the only constructor
//!   (the spec's `new MessagePort()` is a no-op that returns a detached port,
//!   modelled as [`MessagePort::detached`]).
//! - [`MessagePort::post_message`] — the § 9.5.4 `postMessage` steps:
//!   structured-clone the value (honouring the transfer list), enqueue the
//!   clone to the partner's inbox, and re-entangle any transferred ports.
//! - [`MessagePort::drain`] — pop the inbox (the host hook calls this from
//!   the event loop; the queue is empty after).
//!
//! What does *not* live here:
//! - The event-loop delivery (`onmessage` / `addEventListener("message", …)`
//!   firing). Phase 6 host hook.
//! - The JS `MessageEvent` wrapper (the `.data` / `.origin` / `.ports`
//!   surface). Phase 6 host hook.
//! - `MessagePort.start()` / `MessagePort.close()` *reactivity* — the spec
//!   gates delivery on `start()` (called implicitly when `onmessage` is
//!   assigned). Modelled here as a flag; the actual pause/resume is the host
//!   hook.
//!
//! ## Cross-origin isolation
//!
//! Transferring a `SharedArrayBuffer` over a port requires the document's
//! browsing context to be cross-origin-isolated (HTML § 7.2). The
//! `cross_origin_isolated` flag passed to [`MessagePort::post_message`] is
//! the gate [`crate::structured_clone::clone`] consults; this layer passes it
//! through verbatim.
//!
//! Reference: <https://html.spec.whatwg.org/multipage/web-messaging.html#message-ports>.

#![forbid(unsafe_code)]

use crate::structured_clone::{DataCloneError, StructuredCloneValue, Transferable, clone};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// PortId
// ---------------------------------------------------------------------------

/// Opaque handle for a [`MessagePort`]. Carried in
/// [`crate::structured_clone::StructuredCloneValue::MessagePort`] and the
/// [`crate::structured_clone::Transferable::MessagePort`] transfer list. The
/// host hook resolves the id back to a real JS `MessagePort` wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PortId(pub u64);

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// One queued message in a [`MessagePort`]'s inbox. The cloned value + the
/// transferred-port handles re-entangled at the receiver. (The host hook
/// wraps this in a `MessageEvent`.)
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// The structured-cloned `data` payload.
    pub data: StructuredCloneValue,
    /// The transferred ports, re-entangled to *this* port's pair (the
    /// receiver now owns them).
    pub transferred_ports: Vec<PortId>,
}

// ---------------------------------------------------------------------------
// MessagePort
// ---------------------------------------------------------------------------

/// One end of an entangled port pair (HTML § 9.5.2). Cheap to clone for the
/// host hook (the inbox is *not* cloned — the authoritative copy lives in the
/// [`MessageChannel`]).
#[derive(Debug, Clone, PartialEq)]
pub struct MessagePort {
    /// This port's identity.
    pub id: PortId,
    /// The entangled partner's identity (`None` if this port was never
    /// entangled — i.e. constructed via the spec's `new MessagePort()`
    /// no-op).
    pub partner: Option<PortId>,
    /// `true` once `close()` has been called or the port has been transferred
    /// away (§ 9.5.5). A detached port's `postMessage` is a no-op; its inbox
    /// is no longer drained.
    pub detached: bool,
    /// `true` while delivery is enabled (§ 9.5.3 `start()`). The spec auto-
    /// starts when `onmessage` is assigned; the host hook flips this flag.
    pub started: bool,
    /// The queued messages awaiting delivery (the host hook drains via
    /// [`MessagePort::drain`]).
    pub inbox: Vec<Message>,
}

/// The outcome of a successful [`MessagePort::post_message`]: the
/// structured-cloned payload + the partner id to route it to + the port
/// handles re-entangled at the receiver. The host hook uses this to enqueue
/// a [`Message`] on the partner via [`MessagePort::enqueue`].
#[derive(Debug, Clone, PartialEq)]
pub struct PostOutcome {
    /// The partner port the message was routed to.
    pub partner: PortId,
    /// The structured-cloned payload to enqueue at the partner.
    pub data: StructuredCloneValue,
    /// The transferred port handles (re-entangled at the receiver by the
    /// host hook).
    pub transferred_ports: Vec<PortId>,
}

impl MessagePort {
    /// `postMessage(message, transfer)` — the HTML § 9.5.4 steps. Performs
    /// the structured clone of `message` (honouring `transfer`) and returns
    /// the clone + the partner to route it to + the transferred ports.
    ///
    /// Returns `Ok(None)` when the port is detached or unentangled (the spec
    /// silently drops the message in both cases). On error, no side-effect
    /// has been performed (the spec mandates atomicity — a failed clone does
    /// not enqueue a partial message and does not detach a partially-
    /// transferred port).
    ///
    /// The host hook calls [`MessagePort::enqueue`] on the partner with
    /// `outcome.data` + `outcome.transferred_ports`; it calls
    /// [`crate::structured_clone::detach_transferred`] on the source tree to
    /// detach transferred buffers and `MessagePort::transfer_close` on each
    /// transferred port.
    pub fn post_message(
        &mut self,
        message: &StructuredCloneValue,
        transfer: &[Transferable],
        cross_origin_isolated: bool,
        known_platform_types: &HashSet<String>,
    ) -> Result<Option<PostOutcome>, DataCloneError> {
        if self.detached {
            // § 9.5.5: a detached port silently drops the message. The spec
            // returns without error; we mirror that (the host hook may
            // surface a console warning).
            return Ok(None);
        }
        let Some(partner) = self.partner else {
            // An unentangled port (constructed via `new MessagePort()`)
            // likewise drops the message.
            return Ok(None);
        };

        // § 9.5.4 step 3: structured clone with the transfer list. Any error
        // here is propagated before any enqueue / detachment.
        let data = clone(
            message,
            transfer,
            cross_origin_isolated,
            known_platform_types,
        )?;

        // Step 4: collect transferred ports (they will be re-entangled at the
        // receiver). Detaching them locally is the caller's job via
        // [`MessagePort::transfer_close`] after this returns — kept separate
        // so this function stays atomic.
        let transferred_ports: Vec<PortId> = transfer
            .iter()
            .filter_map(|h| match h {
                Transferable::MessagePort(id) => Some(*id),
                _ => None,
            })
            .collect();

        Ok(Some(PostOutcome {
            partner,
            data,
            transferred_ports,
        }))
    }

    /// Enqueue a (already-cloned) message + the transferred ports onto this
    /// port's inbox. Called by the host hook on the *receiver* after the
    /// partner's [`MessagePort::post_message`] returned the cloned value.
    /// Separated from `post_message` so a port never mutates its partner
    /// directly (the two may live in different compartments).
    pub fn enqueue(&mut self, data: StructuredCloneValue, transferred_ports: Vec<PortId>) {
        if self.detached {
            return;
        }
        self.inbox.push(Message {
            data,
            transferred_ports,
        });
    }

    /// Pop every queued message (the host hook calls this from the event
    /// loop). The inbox is empty after. A detached or unstarted port drains
    /// nothing (the spec buffers until `start()`, then flushes).
    pub fn drain(&mut self) -> Vec<Message> {
        if self.detached || !self.started {
            return Vec::new();
        }
        std::mem::take(&mut self.inbox)
    }

    /// § 9.5.3 `start()`. Enables delivery; the next [`Self::drain`] returns
    /// the buffered inbox.
    pub fn start(&mut self) {
        if self.detached {
            return;
        }
        self.started = true;
    }

    /// § 9.5.5 `close()`. Detaches this port; further `postMessage` / drain
    /// are no-ops. The entangled partner is *not* automatically closed (the
    /// spec mandates an explicit `close()` on each side).
    pub fn close(&mut self) {
        self.detached = true;
        self.inbox.clear();
    }

    /// Detach this port because it has been transferred (§ 9.5.4 step 5).
    /// Semantically identical to [`Self::close`] but distinct so the host
    /// hook can distinguish the two for telemetry.
    pub fn transfer_close(&mut self) {
        self.detached = true;
        self.inbox.clear();
    }
}

// ---------------------------------------------------------------------------
// MessageChannel
// ---------------------------------------------------------------------------

/// HTML § 9.5.1 `new MessageChannel()` — the entangled port pair. The two
/// ports share no `&mut` (one is for each side of the channel); the host hook
/// hands one to the sender and the other to the receiver (often across a
/// compartment / worker boundary).
#[derive(Debug, Clone, PartialEq)]
pub struct MessageChannel {
    /// `port1` — typically kept by the constructor.
    pub port1: MessagePort,
    /// `port2` — typically handed to the other side.
    pub port2: MessagePort,
}

impl MessageChannel {
    /// Construct a fresh entangled pair. The two ids are sequential for
    /// testability; the host hook never observes them directly.
    pub fn new(id_a: u64, id_b: u64) -> Self {
        Self {
            port1: MessagePort {
                id: PortId(id_a),
                partner: Some(PortId(id_b)),
                detached: false,
                started: false,
                inbox: Vec::new(),
            },
            port2: MessagePort {
                id: PortId(id_b),
                partner: Some(PortId(id_a)),
                detached: false,
                started: false,
                inbox: Vec::new(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured_clone::Buffer;

    fn no_platform_types() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn channel_ports_are_entangled() {
        let ch = MessageChannel::new(1, 2);
        assert_eq!(ch.port1.id, PortId(1));
        assert_eq!(ch.port1.partner, Some(PortId(2)));
        assert_eq!(ch.port2.id, PortId(2));
        assert_eq!(ch.port2.partner, Some(PortId(1)));
    }

    #[test]
    fn post_message_routes_to_partner_id() {
        let mut ch = MessageChannel::new(1, 2);
        let msg = StructuredCloneValue::String("hi".into());
        let outcome = ch
            .port1
            .post_message(&msg, &[], true, &no_platform_types())
            .unwrap()
            .expect("routed");
        assert_eq!(outcome.partner, PortId(2));
        assert_eq!(outcome.data, msg);
        assert!(outcome.transferred_ports.is_empty());
    }

    #[test]
    fn full_round_trip_enqueues_to_receiver_inbox() {
        let mut ch = MessageChannel::new(1, 2);
        let msg = StructuredCloneValue::Number(42.0);
        // port1 → port2.
        let outcome = ch
            .port1
            .post_message(&msg, &[], true, &no_platform_types())
            .unwrap()
            .expect("routed");
        ch.port2.enqueue(outcome.data, outcome.transferred_ports);

        // Start the receiver and drain.
        ch.port2.start();
        let drained = ch.port2.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].data, msg);
        assert!(drained[0].transferred_ports.is_empty());
    }

    #[test]
    fn drain_returns_nothing_until_started() {
        let mut port = MessagePort {
            id: PortId(1),
            partner: Some(PortId(2)),
            detached: false,
            started: false,
            inbox: vec![Message {
                data: StructuredCloneValue::Null,
                transferred_ports: vec![],
            }],
        };
        assert!(port.drain().is_empty());
        port.start();
        assert_eq!(port.drain().len(), 1);
        // Subsequent drain is empty.
        assert!(port.drain().is_empty());
    }

    #[test]
    fn detached_port_drops_post_message() {
        let mut port = MessagePort {
            id: PortId(1),
            partner: Some(PortId(2)),
            detached: true,
            started: true,
            inbox: Vec::new(),
        };
        let msg = StructuredCloneValue::Null;
        let outcome = port
            .post_message(&msg, &[], true, &no_platform_types())
            .unwrap();
        assert!(outcome.is_none());
    }

    #[test]
    fn close_clears_inbox_and_disables_drain() {
        let mut port = MessagePort {
            id: PortId(1),
            partner: Some(PortId(2)),
            detached: false,
            started: true,
            inbox: vec![Message {
                data: StructuredCloneValue::Null,
                transferred_ports: vec![],
            }],
        };
        port.close();
        assert!(port.detached);
        assert!(port.inbox.is_empty());
        assert!(port.drain().is_empty());
    }

    #[test]
    fn transferred_port_handles_collected() {
        let mut ch = MessageChannel::new(1, 2);
        // port3 to be transferred.
        let port3_id = PortId(3);
        let msg = StructuredCloneValue::Array(vec![StructuredCloneValue::MessagePort(port3_id)]);
        let transfer = vec![Transferable::MessagePort(port3_id)];
        let outcome = ch
            .port1
            .post_message(&msg, &transfer, true, &no_platform_types())
            .unwrap()
            .expect("routed");
        assert_eq!(outcome.partner, PortId(2));
        assert_eq!(outcome.transferred_ports, vec![port3_id]);
    }

    #[test]
    fn shared_buffer_post_requires_isolation_for_clone() {
        let mut ch = MessageChannel::new(1, 2);
        let msg = StructuredCloneValue::ArrayBuffer(Buffer {
            id: 9,
            shared: true,
            detached: false,
        });
        // No transfer → clone path → isolation required.
        let err = ch
            .port1
            .post_message(&msg, &[], false, &no_platform_types())
            .unwrap_err();
        assert_eq!(err, DataCloneError::SharedBufferRequiresIsolation);
    }

    #[test]
    fn shared_buffer_post_transfer_succeeds_without_isolation() {
        let mut ch = MessageChannel::new(1, 2);
        let msg = StructuredCloneValue::ArrayBuffer(Buffer {
            id: 9,
            shared: true,
            detached: false,
        });
        // Transfer path → isolation gate bypassed.
        let outcome = ch
            .port1
            .post_message(
                &msg,
                &[Transferable::SharedArrayBuffer(9)],
                false,
                &no_platform_types(),
            )
            .unwrap()
            .expect("routed");
        assert_eq!(outcome.partner, PortId(2));
    }

    #[test]
    fn post_message_atomic_on_clone_error() {
        let mut ch = MessageChannel::new(1, 2);
        // An unregistered platform object triggers a clone error.
        let msg = StructuredCloneValue::PlatformObject("File".into());
        let err = ch
            .port1
            .post_message(&msg, &[], true, &no_platform_types())
            .unwrap_err();
        assert!(matches!(err, DataCloneError::UnsupportedPlatformObject(_)));
        // No enqueue happened; the receiver's inbox is empty.
        assert!(ch.port2.inbox.is_empty());
    }

    #[test]
    fn unentangled_port_drops_post_message() {
        let mut port = MessagePort {
            id: PortId(1),
            partner: None, // never entangled (`new MessagePort()` no-op)
            detached: false,
            started: true,
            inbox: Vec::new(),
        };
        let msg = StructuredCloneValue::Null;
        let outcome = port
            .post_message(&msg, &[], true, &no_platform_types())
            .unwrap();
        assert!(outcome.is_none());
    }

    #[test]
    fn transfer_close_marks_detached() {
        let mut port = MessagePort {
            id: PortId(1),
            partner: Some(PortId(2)),
            detached: false,
            started: true,
            inbox: vec![Message {
                data: StructuredCloneValue::Null,
                transferred_ports: vec![],
            }],
        };
        port.transfer_close();
        assert!(port.detached);
        assert!(port.inbox.is_empty());
    }

    #[test]
    fn port_id_equality_and_hash() {
        let a = PortId(7);
        let b = PortId(7);
        let c = PortId(8);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }
}
