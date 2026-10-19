//! Process-layer envelopes: what travels inside [`Frame::Data`].
//!
//! The transport layer sees opaque bytes; this module defines their shape.
//! Payloads (`args`, `msg`, `reply`) are themselves postcard-encoded typed
//! values, decoded at the trust boundary by the [`crate::registry`] entry
//! for the target behavior — “parse, don't validate” at every node
//! boundary (ADR 0009 § wire protocol).
//!
//! [`Frame::Data`]: crate::wire::Frame::Data

use std::sync::Arc;

use id_effect::{Span, SpanHook};
use id_effect_process::{ExitReason, NodeName, Pid};
use serde::{Deserialize, Serialize};

use crate::global_registry::NameClaim;

/// Wire form of [`Pid`] (the process crate stays serde-free).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PidWire {
  /// Logical node name (`name@host`).
  pub node: String,
  /// Node-local process id.
  pub id: u64,
  /// Node incarnation the pid was minted under.
  pub incarnation: u32,
}

impl From<&Pid> for PidWire {
  fn from(pid: &Pid) -> Self {
    Self {
      node: pid.node().as_str().to_string(),
      id: pid.local_id(),
      incarnation: pid.incarnation(),
    }
  }
}

impl PidWire {
  /// Rebuild a [`Pid`] from its wire form.
  pub fn to_pid(&self) -> Pid {
    Pid::from_parts(NodeName::new(self.node.as_str()), self.id, self.incarnation)
  }
}

/// Wire form of [`ExitReason`] — exit reasons are plain data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExitReasonWire {
  /// Graceful completion.
  Normal,
  /// Graceful supervisor stop.
  Shutdown,
  /// Abnormal termination, rendered to a string.
  Error(String),
  /// Untrappable kill.
  Killed,
  /// Fiber interrupted.
  Interrupted,
  /// Target did not exist.
  NoProc,
  /// Hosting node unreachable.
  NoConnection,
  /// Lost a cluster-wide name to a conflicting registration.
  NameConflict(String),
}

impl From<&ExitReason> for ExitReasonWire {
  fn from(reason: &ExitReason) -> Self {
    match reason {
      ExitReason::Normal => Self::Normal,
      ExitReason::Shutdown => Self::Shutdown,
      ExitReason::Error(e) => Self::Error(e.clone()),
      ExitReason::Killed => Self::Killed,
      ExitReason::Interrupted => Self::Interrupted,
      ExitReason::NoProc => Self::NoProc,
      ExitReason::NoConnection => Self::NoConnection,
      ExitReason::NameConflict(name) => Self::NameConflict(name.clone()),
      // `ExitReason` is non-exhaustive: a reason this wire version cannot name travels as its
      // rendering, still abnormal.
      other => Self::Error(other.to_string()),
    }
  }
}

impl From<&ExitReasonWire> for ExitReason {
  fn from(reason: &ExitReasonWire) -> Self {
    match reason {
      ExitReasonWire::Normal => Self::Normal,
      ExitReasonWire::Shutdown => Self::Shutdown,
      ExitReasonWire::Error(e) => Self::Error(e.clone()),
      ExitReasonWire::Killed => Self::Killed,
      ExitReasonWire::Interrupted => Self::Interrupted,
      ExitReasonWire::NoProc => Self::NoProc,
      ExitReasonWire::NoConnection => Self::NoConnection,
      ExitReasonWire::NameConflict(name) => Self::NameConflict(name.clone()),
    }
  }
}

/// W3C-style trace context propagated with cross-node messages (consumed
/// by `id_effect_opentelemetry::propagation`; empty when tracing is off).
///
/// Inject with `id_effect_opentelemetry::propagation::inject_current_trace_context`
/// into a mutable `TraceContext`; extract with `extract_trace_context_from_headers`.
/// The mesh fills it from the sending fiber's span through its [`TracePropagator`].
pub type TraceContext = Vec<(String, String)>;

/// Empty trace bag.
pub fn empty_trace() -> TraceContext {
  Vec::new()
}

/// Single header pair (tests / manual propagation).
pub fn trace_pair(key: impl Into<String>, value: impl Into<String>) -> TraceContext {
  vec![(key.into(), value.into())]
}

/// The W3C header naming the parent span.
pub const TRACEPARENT: &str = "traceparent";

/// `span` as a W3C `traceparent` value (`00-<trace id>-<span id>-01`), from its id_effect ids.
pub fn traceparent_of(span: &Span) -> String {
  format!("00-{:032x}-{:016x}-01", span.trace_id().0, span.id().0)
}

/// The `traceparent` value carried in `trace`, if any (header names are case-insensitive).
pub fn traceparent_in(trace: &TraceContext) -> Option<&str> {
  trace
    .iter()
    .find(|(k, _)| k.eq_ignore_ascii_case(TRACEPARENT))
    .map(|(_, v)| v.as_str())
}

/// How spans cross the wire (dist.remote#8): the sender writes the context of its fiber's
/// current span into the message's [`TraceContext`], and the receiver opens the span that handles
/// the message (`mesh.call`, `mesh.cast`, `mesh.info`) under the hook this propagator returns for
/// that context, so the remote span is its parent.
///
/// The default, [`W3cTraceContext`], uses id_effect's own span ids. With OpenTelemetry, implement
/// it with `id_effect_opentelemetry`: inject `span_otel_context(span)` with
/// `inject_trace_context_into_headers`, and return
/// `OtelSpanHook::with_parent(extract_trace_context_from_headers(..))` as the receiver hook.
pub trait TracePropagator: Send + Sync + 'static {
  /// Write `span`'s context into `carrier` (sending side).
  fn inject(&self, span: &Span, carrier: &mut TraceContext);

  /// The span hook for the receiving span of a message that carried `carrier`; `None` opens no
  /// span. The receiving span always has the attribute `mesh.parent` (the carried
  /// `traceparent`).
  fn receiver_hook(&self, carrier: &TraceContext) -> Option<Arc<dyn SpanHook>>;
}

/// The default [`TracePropagator`]: a W3C `traceparent` built from id_effect span ids
/// ([`traceparent_of`]); receiving spans go to the hook given with
/// [`W3cTraceContext::with_receiver_hook`] (none by default), naming the remote parent in their
/// `mesh.parent` attribute.
#[derive(Clone, Default)]
pub struct W3cTraceContext {
  hook: Option<Arc<dyn SpanHook>>,
}

impl W3cTraceContext {
  /// Inject only; receiving spans are not opened.
  pub fn new() -> Self {
    Self::default()
  }

  /// Open receiving spans under `hook`.
  pub fn with_receiver_hook(mut self, hook: Arc<dyn SpanHook>) -> Self {
    self.hook = Some(hook);
    self
  }
}

impl std::fmt::Debug for W3cTraceContext {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("W3cTraceContext")
      .field("receiver_hook", &self.hook.is_some())
      .finish()
  }
}

impl TracePropagator for W3cTraceContext {
  fn inject(&self, span: &Span, carrier: &mut TraceContext) {
    carrier.retain(|(k, _)| !k.eq_ignore_ascii_case(TRACEPARENT));
    carrier.push((TRACEPARENT.to_string(), traceparent_of(span)));
  }

  fn receiver_hook(&self, _carrier: &TraceContext) -> Option<Arc<dyn SpanHook>> {
    self.hook.clone()
  }
}

/// A message sent at-least-once ([`ProcessEnvelope::Reliable`]): only casts and bare messages
/// are, calls keep their own reply and timeout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ReliableMessage {
  /// As [`ProcessEnvelope::Cast`].
  Cast {
    /// Target process.
    to: PidWire,
    /// Postcard-encoded `P::Cast`.
    msg: Vec<u8>,
    /// Distributed tracing context.
    trace: TraceContext,
  },
  /// As [`ProcessEnvelope::Info`].
  Info {
    /// Target process.
    to: PidWire,
    /// Postcard-encoded `P::Info`.
    msg: Vec<u8>,
    /// Distributed tracing context.
    trace: TraceContext,
  },
}

impl ReliableMessage {
  /// The target process.
  pub fn to(&self) -> &PidWire {
    match self {
      Self::Cast { to, .. } | Self::Info { to, .. } => to,
    }
  }

  /// Payload size in bytes (what the retransmit buffer's byte bound counts).
  pub fn payload_len(&self) -> usize {
    match self {
      Self::Cast { msg, trace, .. } | Self::Info { msg, trace, .. } => {
        msg.len() + trace.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>()
      }
    }
  }

  /// The at-most-once envelope with the same effect on the receiver.
  pub fn into_envelope(self) -> ProcessEnvelope {
    match self {
      Self::Cast { to, msg, trace } => ProcessEnvelope::Cast { to, msg, trace },
      Self::Info { to, msg, trace } => ProcessEnvelope::Info { to, msg, trace },
    }
  }
}

/// Everything the process layer sends between nodes.
///
/// Delivery contract (normative, ADR 0009): at-most-once, ordered per
/// connection epoch, dropped on disconnect. `Call`/`Spawn` correlate
/// request and reply by id; ids are unique per (node, boot). Casts and bare
/// messages sent at-least-once travel as [`ProcessEnvelope::Reliable`] and are
/// acknowledged with [`ProcessEnvelope::Ack`] (protocol v3, see [`crate::delivery`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ProcessEnvelope {
  /// Fire-and-forget cast to a remote process.
  Cast {
    /// Target process.
    to: PidWire,
    /// Postcard-encoded `P::Cast`.
    msg: Vec<u8>,
    /// Distributed tracing context.
    trace: TraceContext,
  },
  /// Bare message delivered to `handle_info`.
  Info {
    /// Target process.
    to: PidWire,
    /// Postcard-encoded `P::Info`.
    msg: Vec<u8>,
    /// Distributed tracing context.
    trace: TraceContext,
  },
  /// Synchronous request expecting a [`ProcessEnvelope::Reply`].
  Call {
    /// Target process.
    to: PidWire,
    /// Correlation id, unique per sender boot.
    call_id: u64,
    /// Postcard-encoded `P::Call`.
    msg: Vec<u8>,
    /// Distributed tracing context.
    trace: TraceContext,
  },
  /// Response to a [`ProcessEnvelope::Call`].
  Reply {
    /// Correlation id of the originating call.
    call_id: u64,
    /// `Ok(postcard P::Reply)` or a rendered failure.
    result: Result<Vec<u8>, RemoteFault>,
  },
  /// Spawn a registered behavior on the receiving node.
  Spawn {
    /// Correlation id, unique per sender boot.
    spawn_id: u64,
    /// Behavior name in the receiver's [`crate::registry::BehaviorRegistry`].
    behavior: String,
    /// Postcard-encoded `P::Args`.
    args: Vec<u8>,
    /// Pid on the *requesting* node to link the child to (remote link),
    /// if any.
    link_to: Option<PidWire>,
  },
  /// Response to a [`ProcessEnvelope::Spawn`].
  SpawnReply {
    /// Correlation id of the originating spawn.
    spawn_id: u64,
    /// The new pid, or why the spawn failed.
    result: Result<PidWire, RemoteFault>,
  },
  /// Set up a monitor: watcher on the sending node, target on the
  /// receiving node. The receiver answers with `Down` exactly once.
  Monitor {
    /// Wire-unique monitor reference (sender-allocated).
    monitor: u64,
    /// Target process on the receiving node.
    target: PidWire,
  },
  /// Tear down a monitor before it fires.
  Demonitor {
    /// Reference from the original `Monitor`.
    monitor: u64,
    /// Target process it was watching.
    target: PidWire,
  },
  /// Monitor fired: the watched process exited.
  Down {
    /// Reference from the original `Monitor`.
    monitor: u64,
    /// The process that exited.
    target: PidWire,
    /// Why it exited.
    reason: ExitReasonWire,
  },
  /// Link two processes across nodes (bidirectional fate-sharing).
  Link {
    /// Process on the sending node.
    from: PidWire,
    /// Process on the receiving node.
    to: PidWire,
  },
  /// Remove a cross-node link.
  Unlink {
    /// Process on the sending node.
    from: PidWire,
    /// Process on the receiving node.
    to: PidWire,
  },
  /// Exit signal crossing the wire (link propagation, remote `exit/2`).
  ExitSignal {
    /// Process the signal is about.
    from: PidWire,
    /// Process to deliver the signal to.
    to: PidWire,
    /// Why.
    reason: ExitReasonWire,
  },
  /// Claim a cluster-wide name (sent by the node hosting the process).
  GlobalRegister {
    /// The registration.
    claim: NameClaim,
  },
  /// Release a cluster-wide name held by `pid`.
  GlobalUnregister {
    /// The released name.
    name: String,
    /// The process that held it.
    pid: PidWire,
  },
  /// Every name the sending node owns; replaces what the receiver held for that node.
  GlobalSync {
    /// The sender's claims.
    claims: Vec<NameClaim>,
  },
  /// An at-least-once message: the `seq`-th reliable message of the sending node's life to
  /// this peer. The receiver delivers it once per (sender incarnation, `seq`) and answers
  /// with an [`ProcessEnvelope::Ack`]; the sender retransmits it until then.
  Reliable {
    /// Per-peer sequence number (from 1).
    seq: u64,
    /// Every sequence number below this one is acknowledged or abandoned by the sender, so the
    /// receiver's dedup window may slide past it.
    floor: u64,
    /// The message.
    message: ReliableMessage,
  },
  /// Receipt of reliable messages from the receiving node.
  Ack {
    /// Every sequence number up to this one has been received.
    upto: u64,
    /// The sequence number that provoked this ack (received, possibly above `upto`).
    seq: u64,
  },
}

/// Why a remote operation failed, as data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteFault {
  /// Target process does not exist (or already exited).
  NoProc,
  /// No behavior registered under the requested name.
  UnknownBehavior(String),
  /// Payload failed typed decode at the trust boundary.
  Decode(String),
  /// The callee exited before replying.
  Down(ExitReasonWire),
  /// The callee's node-side call timed out.
  Timeout,
  /// Behavior init failed.
  SpawnFailed(String),
  /// The target's mailbox is full; the call was not enqueued.
  MailboxFull,
}

impl std::fmt::Display for RemoteFault {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      RemoteFault::NoProc => f.write_str("noproc"),
      RemoteFault::UnknownBehavior(name) => write!(f, "unknown behavior: {name}"),
      RemoteFault::Decode(e) => write!(f, "decode: {e}"),
      RemoteFault::Down(r) => write!(f, "down: {r:?}"),
      RemoteFault::Timeout => f.write_str("timeout"),
      RemoteFault::SpawnFailed(e) => write!(f, "spawn failed: {e}"),
      RemoteFault::MailboxFull => f.write_str("mailbox full"),
    }
  }
}

/// Encode an envelope for a [`Frame::Data`] payload.
///
/// [`Frame::Data`]: crate::wire::Frame::Data
pub fn encode_envelope(envelope: &ProcessEnvelope) -> Result<Vec<u8>, postcard::Error> {
  postcard::to_allocvec(envelope)
}

/// Decode a [`Frame::Data`] payload. Garbage fails cleanly.
///
/// [`Frame::Data`]: crate::wire::Frame::Data
pub fn decode_envelope(bytes: &[u8]) -> Result<ProcessEnvelope, postcard::Error> {
  postcard::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn envelopes_round_trip() {
    let pid = PidWire {
      node: "a@host".to_string(),
      id: 7,
      incarnation: 1,
    };
    let cases = vec![
      ProcessEnvelope::Cast {
        to: pid.clone(),
        msg: vec![1, 2],
        trace: vec![("traceparent".to_string(), "00-abc".to_string())],
      },
      ProcessEnvelope::Call {
        to: pid.clone(),
        call_id: 9,
        msg: vec![3],
        trace: Vec::new(),
      },
      ProcessEnvelope::Reply {
        call_id: 9,
        result: Err(RemoteFault::Timeout),
      },
      ProcessEnvelope::Down {
        monitor: 4,
        target: pid,
        reason: ExitReasonWire::NoConnection,
      },
    ];
    for envelope in cases {
      let bytes = encode_envelope(&envelope).expect("encode");
      assert_eq!(decode_envelope(&bytes).expect("decode"), envelope);
    }
  }

  fn wire_pid(id: u64) -> PidWire {
    PidWire {
      node: "b@host".to_string(),
      id,
      incarnation: 3,
    }
  }

  #[test]
  fn control_envelopes_round_trip() {
    let cases = vec![
      ProcessEnvelope::Info {
        to: wire_pid(1),
        msg: vec![9],
        trace: empty_trace(),
      },
      ProcessEnvelope::Spawn {
        spawn_id: 2,
        behavior: "counter".to_string(),
        args: vec![0],
        link_to: Some(wire_pid(5)),
      },
      ProcessEnvelope::SpawnReply {
        spawn_id: 2,
        result: Ok(wire_pid(6)),
      },
      ProcessEnvelope::SpawnReply {
        spawn_id: 3,
        result: Err(RemoteFault::UnknownBehavior("nope".to_string())),
      },
      ProcessEnvelope::Monitor {
        monitor: 8,
        target: wire_pid(1),
      },
      ProcessEnvelope::Demonitor {
        monitor: 8,
        target: wire_pid(1),
      },
      ProcessEnvelope::Link {
        from: wire_pid(1),
        to: wire_pid(2),
      },
      ProcessEnvelope::Unlink {
        from: wire_pid(1),
        to: wire_pid(2),
      },
      ProcessEnvelope::ExitSignal {
        from: wire_pid(1),
        to: wire_pid(2),
        reason: ExitReasonWire::Error("boom".to_string()),
      },
      ProcessEnvelope::Reply {
        call_id: 4,
        result: Ok(vec![1, 2, 3]),
      },
    ];
    for envelope in cases {
      let bytes = encode_envelope(&envelope).expect("encode");
      assert_eq!(decode_envelope(&bytes).expect("decode"), envelope);
    }
  }

  #[test]
  fn global_name_envelopes_round_trip() {
    let claim = NameClaim {
      name: "svc".to_string(),
      pid: wire_pid(1),
      owner: crate::id::NodeId::from_parts("b@host", 7),
    };
    let cases = vec![
      ProcessEnvelope::GlobalRegister {
        claim: claim.clone(),
      },
      ProcessEnvelope::GlobalUnregister {
        name: "svc".to_string(),
        pid: wire_pid(1),
      },
      ProcessEnvelope::GlobalSync {
        claims: vec![claim],
      },
      ProcessEnvelope::GlobalSync { claims: Vec::new() },
    ];
    for envelope in cases {
      let bytes = encode_envelope(&envelope).expect("encode");
      assert_eq!(decode_envelope(&bytes).expect("decode"), envelope);
    }
  }

  #[test]
  fn reliable_envelopes_round_trip() {
    let cases = vec![
      ProcessEnvelope::Reliable {
        seq: 3,
        floor: 1,
        message: ReliableMessage::Cast {
          to: wire_pid(1),
          msg: vec![1],
          trace: trace_pair(TRACEPARENT, "00-ab-cd-01"),
        },
      },
      ProcessEnvelope::Reliable {
        seq: u64::MAX,
        floor: u64::MAX,
        message: ReliableMessage::Info {
          to: wire_pid(2),
          msg: Vec::new(),
          trace: empty_trace(),
        },
      },
      ProcessEnvelope::Ack { upto: 2, seq: 3 },
    ];
    for envelope in cases {
      let bytes = encode_envelope(&envelope).expect("encode");
      assert_eq!(decode_envelope(&bytes).expect("decode"), envelope);
    }
  }

  #[test]
  fn reliable_message_unwraps_to_its_at_most_once_envelope() {
    let cast = ReliableMessage::Cast {
      to: wire_pid(1),
      msg: vec![1, 2],
      trace: trace_pair("k", "vv"),
    };
    assert_eq!(cast.to(), &wire_pid(1));
    assert_eq!(cast.payload_len(), 5);
    assert_eq!(
      cast.into_envelope(),
      ProcessEnvelope::Cast {
        to: wire_pid(1),
        msg: vec![1, 2],
        trace: trace_pair("k", "vv"),
      }
    );
    let info = ReliableMessage::Info {
      to: wire_pid(2),
      msg: vec![3],
      trace: empty_trace(),
    };
    assert_eq!(info.to(), &wire_pid(2));
    assert!(matches!(
      info.into_envelope(),
      ProcessEnvelope::Info { msg, .. } if msg == vec![3]
    ));
  }

  #[test]
  fn w3c_propagator_writes_one_traceparent_from_the_span_ids() {
    let eff: id_effect::Effect<Option<Span>, ()> = Span::current().with_span("send");
    let span = match eff.block_on() {
      id_effect::Exit::Success(Some(span)) => span,
      _ => panic!("no current span"),
    };
    let value = traceparent_of(&span);
    assert_eq!(
      value,
      format!("00-{:032x}-{:016x}-01", span.trace_id().0, span.id().0)
    );
    let mut carrier = vec![("TraceParent".to_string(), "stale".to_string())];
    let propagator = W3cTraceContext::new();
    propagator.inject(&span, &mut carrier);
    assert_eq!(carrier, trace_pair(TRACEPARENT, value.clone()));
    assert_eq!(traceparent_in(&carrier), Some(value.as_str()));
    assert_eq!(traceparent_in(&empty_trace()), None);
    assert!(propagator.receiver_hook(&carrier).is_none());
    let hooked = W3cTraceContext::new().with_receiver_hook(id_effect::tracing_hook());
    assert!(hooked.receiver_hook(&carrier).is_some());
    assert!(format!("{hooked:?}").contains("receiver_hook: true"));
  }

  #[test]
  fn garbage_envelope_fails_cleanly() {
    assert!(decode_envelope(&[0xff, 0xff, 0xff]).is_err());
    assert!(decode_envelope(&[]).is_err());
  }

  #[test]
  fn pid_wire_round_trips_through_pid() {
    let pid = Pid::from_parts(NodeName::new("a@host"), 42, 7);
    let wire = PidWire::from(&pid);
    assert_eq!(wire.node, "a@host");
    assert_eq!(wire.id, 42);
    assert_eq!(wire.incarnation, 7);
    assert_eq!(wire.to_pid(), pid);
  }

  #[test]
  fn exit_reasons_round_trip_through_wire() {
    let reasons = [
      ExitReason::Normal,
      ExitReason::Shutdown,
      ExitReason::Error("boom".to_string()),
      ExitReason::Killed,
      ExitReason::Interrupted,
      ExitReason::NoProc,
      ExitReason::NoConnection,
      ExitReason::NameConflict("svc".to_string()),
    ];
    for reason in reasons {
      let wire = ExitReasonWire::from(&reason);
      assert_eq!(ExitReason::from(&wire), reason);
    }
  }

  #[test]
  fn remote_fault_display_covers_every_variant() {
    let cases = [
      (RemoteFault::NoProc, "noproc"),
      (
        RemoteFault::UnknownBehavior("w".to_string()),
        "unknown behavior: w",
      ),
      (RemoteFault::Decode("eof".to_string()), "decode: eof"),
      (RemoteFault::Down(ExitReasonWire::Killed), "down: Killed"),
      (RemoteFault::Timeout, "timeout"),
      (
        RemoteFault::SpawnFailed("init".to_string()),
        "spawn failed: init",
      ),
    ];
    for (fault, text) in cases {
      assert_eq!(fault.to_string(), text);
    }
  }

  #[test]
  fn trace_helpers_build_header_bags() {
    assert!(empty_trace().is_empty());
    assert_eq!(
      trace_pair("traceparent", "00-abc"),
      vec![("traceparent".to_string(), "00-abc".to_string())]
    );
  }
}
