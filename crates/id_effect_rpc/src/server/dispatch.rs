//! Transport-independent dispatch: tag (and version) lookup, the procedure's guards on the
//! request head, then the call. The HTTP edge ([`super::RpcServer`]), the in-process transport
//! and the later transports (WebSocket, SSE, WebTransport, mesh) all go through here, so every
//! transport enforces the same access decision in the same order — outermost (last added) guard
//! first — and runs calls under the same deadline and cancellation rules.
//!
//! - [`open`] = [`resolve`] + [`authorize`]: the entry a transport may call, or a [`Rejection`].
//! - [`dispatch`]: a whole unary call ([`WireCall`] in, [`WireResult`] out).
//! - `start_*`: start a call of each kind on an opened entry, under a [`CallControl`]; streams
//!   come back as [`CallFrames`].

use std::fmt;

use axum::http::request::Parts;
use axum::response::Response;
use serde_json::Value;

use crate::cancel::{CallControl, controlled};
use crate::contract::ProcedureKind;
use crate::handler::{
  BoxCall, CallContext, CallOutcome, ErasedHandler, InboundSender, inbound, outbound,
};
use crate::kinds::CallFrames;
use crate::registry::{RpcGroup, RpcMethodEntry};
use crate::wire::{CallId, Cursor, WireCall, WireResult};

/// Why a call was not run (each transport renders it: HTTP as a Problem).
pub enum Rejection {
  /// No procedure has this tag, or not at the requested version (HTTP 404).
  UnknownProcedure {
    /// The tag the caller sent.
    tag: String,
    /// The version the caller asked for.
    version: Option<u32>,
  },
  /// A guard answered instead of the handler (its own response, typically 401 or 403).
  Refused(Box<Response>),
  /// The procedure exists but is not of a kind this entry point calls (HTTP 400; `bidi` over
  /// plain HTTP is 426).
  WrongKind {
    /// The tag.
    tag: String,
    /// The procedure's kind.
    kind: ProcedureKind,
  },
}

impl fmt::Debug for Rejection {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::UnknownProcedure { tag, version } => f
        .debug_struct("UnknownProcedure")
        .field("tag", tag)
        .field("version", version)
        .finish(),
      Self::Refused(response) => f.debug_tuple("Refused").field(&response.status()).finish(),
      Self::WrongKind { tag, kind } => f
        .debug_struct("WrongKind")
        .field("tag", tag)
        .field("kind", kind)
        .finish(),
    }
  }
}

/// The entry for `tag` at `version` (newest when `None`).
pub fn resolve<'g>(
  group: &'g RpcGroup,
  tag: &str,
  version: Option<u32>,
) -> Result<&'g RpcMethodEntry, Rejection> {
  let entry = match version {
    Some(v) => group.get_version(tag, v),
    None => group.get(tag),
  };
  entry.ok_or_else(|| Rejection::UnknownProcedure {
    tag: tag.to_owned(),
    version,
  })
}

/// Run `entry`'s guards on the request head, outermost (last added) first.
pub async fn authorize(entry: &RpcMethodEntry, parts: &Parts) -> Result<(), Rejection> {
  for guard in entry.guards().iter().rev() {
    guard
      .check(parts)
      .await
      .map_err(|response| Rejection::Refused(Box::new(response)))?;
  }
  Ok(())
}

/// [`resolve`] then [`authorize`]: the entry the caller may call.
pub async fn open<'g>(
  group: &'g RpcGroup,
  parts: &Parts,
  tag: &str,
  version: Option<u32>,
) -> Result<&'g RpcMethodEntry, Rejection> {
  let entry = resolve(group, tag, version)?;
  authorize(entry, parts).await?;
  Ok(entry)
}

fn wrong_kind(entry: &RpcMethodEntry) -> Rejection {
  Rejection::WrongKind {
    tag: entry.tag.clone(),
    kind: entry.kind(),
  }
}

/// Start a query or mutation.
pub fn start_unary(
  entry: &RpcMethodEntry,
  payload: Value,
  ctx: CallContext,
  control: CallControl,
) -> Result<BoxCall, Rejection> {
  let ErasedHandler::Unary(call) = entry.handler() else {
    return Err(wrong_kind(entry));
  };
  let call = call(payload, ctx);
  Ok(Box::pin(controlled(call, control)))
}

/// Start a server stream whose frames carry `id`; at most `window` items wait unread.
pub fn start_server_stream(
  entry: &RpcMethodEntry,
  id: CallId,
  payload: Value,
  ctx: CallContext,
  control: CallControl,
  window: usize,
) -> Result<CallFrames, Rejection> {
  let ErasedHandler::ServerStream(call) = entry.handler() else {
    return Err(wrong_kind(entry));
  };
  let (out, items) = outbound(window);
  let run = call(payload, ctx, out);
  Ok(CallFrames::new(
    id,
    Box::pin(controlled(run, control)),
    items,
  ))
}

/// Start a client stream: send its items through the sender (drop it to end the input), await
/// the call. At most `capacity` items wait undecoded.
pub fn start_client_stream(
  entry: &RpcMethodEntry,
  ctx: CallContext,
  control: CallControl,
  capacity: usize,
) -> Result<(InboundSender, BoxCall), Rejection> {
  let ErasedHandler::ClientStream(call) = entry.handler() else {
    return Err(wrong_kind(entry));
  };
  let (tx, items) = inbound(capacity);
  let run = call(items, ctx);
  Ok((tx, Box::pin(controlled(run, control))))
}

/// Start a two-way stream whose frames carry `id`.
pub fn start_bidi(
  entry: &RpcMethodEntry,
  id: CallId,
  ctx: CallContext,
  control: CallControl,
  window: usize,
) -> Result<(InboundSender, CallFrames), Rejection> {
  let ErasedHandler::Bidi(call) = entry.handler() else {
    return Err(wrong_kind(entry));
  };
  let (tx, items) = inbound(window);
  let (out, frames) = outbound(window);
  let run = call(items, ctx, out);
  Ok((
    tx,
    CallFrames::new(id, Box::pin(controlled(run, control)), frames),
  ))
}

/// Start a subscription whose frames carry `id`, resuming after `after`.
pub fn start_subscription(
  entry: &RpcMethodEntry,
  id: CallId,
  payload: Value,
  after: Option<Cursor>,
  ctx: CallContext,
  control: CallControl,
  window: usize,
) -> Result<CallFrames, Rejection> {
  let ErasedHandler::Subscription(call) = entry.handler() else {
    return Err(wrong_kind(entry));
  };
  let (out, items) = outbound(window);
  let run = call(payload, after, ctx, out);
  Ok(CallFrames::new(
    id,
    Box::pin(controlled(run, control)),
    items,
  ))
}

/// A unary call's [`WireResult`] and the HTTP status of a single call.
#[derive(Debug, Clone, PartialEq)]
pub struct Dispatched {
  /// The result, id and tag echoed.
  pub result: WireResult,
  /// The status (a batch ignores it: always 200).
  pub status: http::StatusCode,
}

/// Look up `call.tag` (at `call.v`) in `group`, authorize it against `parts`, and run it with
/// `ctx` under `control`. Only queries and mutations are dispatched here.
pub async fn dispatch(
  group: &RpcGroup,
  parts: &Parts,
  call: WireCall,
  ctx: CallContext,
  control: CallControl,
) -> Result<Dispatched, Rejection> {
  let entry = open(group, parts, &call.tag, call.v).await?;
  let run = start_unary(entry, call.payload, ctx, control)?;
  let CallOutcome { exit, status } = run.await;
  Ok(Dispatched {
    result: WireResult::new(call.id, call.tag, exit),
    status,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::registry::{Procedure, RouteGuard};
  use crate::wire::WireExit;
  use axum::http::{Request, StatusCode};
  use axum::response::IntoResponse;
  use id_effect::{DynEnv, schema, succeed};
  use id_effect_axum::route::GuardFuture;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  /// Records the order guards run in; refuses when `refuse`.
  struct Probe {
    name: &'static str,
    refuse: bool,
    order: Arc<std::sync::Mutex<Vec<&'static str>>>,
    calls: Arc<AtomicUsize>,
  }

  impl RouteGuard for Probe {
    fn policy(&self) -> &str {
      self.name
    }

    fn check<'a>(&'a self, _parts: &'a Parts) -> GuardFuture<'a> {
      self.order.lock().unwrap().push(self.name);
      self.calls.fetch_add(1, Ordering::SeqCst);
      let refuse = self.refuse;
      Box::pin(async move {
        if refuse {
          Err(StatusCode::FORBIDDEN.into_response())
        } else {
          Ok(())
        }
      })
    }
  }

  fn parts() -> Parts {
    Request::new(()).into_parts().0
  }

  fn call(tag: &str) -> WireCall {
    WireCall::new(tag, serde_json::json!("Ada")).with_id(CallId::new(3).unwrap())
  }

  fn ctx() -> CallContext {
    CallContext::new(DynEnv::new(), "cid")
  }

  fn strings() -> Arc<id_effect::schema::Schema<String, String>> {
    Arc::new(schema::string::<()>())
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn guards_run_outermost_first_and_can_refuse() {
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let probe = |name, refuse| Probe {
      name,
      refuse,
      order: order.clone(),
      calls: calls.clone(),
    };
    let mut group = RpcGroup::new();
    group.register(
      Procedure::query("A.open")
        .guard(probe("inner", false))
        .guard(probe("outer", false)),
      strings(),
      strings(),
      &["name"],
      |name, _| succeed(name),
    );
    group.register(
      Procedure::query("A.closed")
        .guard(probe("never", false))
        .guard(probe("deny", true)),
      strings(),
      strings(),
      &["name"],
      |name, _| succeed(name),
    );
    let ok = dispatch(&group, &parts(), call("A.open"), ctx(), CallControl::none())
      .await
      .unwrap();
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.result.id, CallId::new(3).ok());
    assert!(matches!(ok.result.exit, WireExit::Success { .. }));
    assert_eq!(*order.lock().unwrap(), ["outer", "inner"]);

    let refused = dispatch(
      &group,
      &parts(),
      call("A.closed"),
      ctx(),
      CallControl::none(),
    )
    .await;
    let Err(Rejection::Refused(response)) = refused else {
      panic!("expected a refusal");
    };
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "the inner guard never ran");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn unknown_tags_versions_and_kinds_are_rejections() {
    let mut group = RpcGroup::new();
    group.register(
      Procedure::query("A.n").public(),
      strings(),
      strings(),
      &["text"],
      |n, _| succeed(n),
    );
    let unknown = dispatch(&group, &parts(), call("A.nope"), ctx(), CallControl::none()).await;
    let err = unknown.unwrap_err();
    assert!(format!("{err:?}").contains("A.nope"));
    assert!(matches!(err, Rejection::UnknownProcedure { tag, version: None } if tag == "A.nope"));
    let v9 = dispatch(
      &group,
      &parts(),
      call("A.n").with_version(9),
      ctx(),
      CallControl::none(),
    )
    .await;
    assert!(matches!(
      v9,
      Err(Rejection::UnknownProcedure {
        version: Some(9),
        ..
      })
    ));
    let entry = resolve(&group, "A.n", Some(1)).unwrap();
    let ctl = CallControl::none;
    for err in [
      start_server_stream(
        entry,
        CallId::CONNECTION,
        serde_json::Value::Null,
        ctx(),
        ctl(),
        1,
      )
      .map(|_| ())
      .unwrap_err(),
      start_client_stream(entry, ctx(), ctl(), 1)
        .map(|_| ())
        .unwrap_err(),
      start_bidi(entry, CallId::CONNECTION, ctx(), ctl(), 1)
        .map(|_| ())
        .unwrap_err(),
      start_subscription(
        entry,
        CallId::CONNECTION,
        serde_json::Value::Null,
        None,
        ctx(),
        ctl(),
        1,
      )
      .map(|_| ())
      .unwrap_err(),
    ] {
      assert!(matches!(
        err,
        Rejection::WrongKind {
          kind: ProcedureKind::Query,
          ..
        }
      ));
      assert!(format!("{err:?}").starts_with("WrongKind"));
    }
    let refused = Rejection::Refused(Box::new(StatusCode::UNAUTHORIZED.into_response()));
    assert!(format!("{refused:?}").contains("401"));
  }
}
