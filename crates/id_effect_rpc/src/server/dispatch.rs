//! Transport-independent dispatch of one call: tag lookup, the procedure's guards on the request
//! head, then the handler. The HTTP edge ([`super::RpcServer`]) and the later transports
//! (WebSocket, SSE, mesh) share it, so every transport enforces the same access decision.

use std::fmt;

use axum::http::request::Parts;
use axum::response::Response;
use id_effect::DynEnv;

use crate::protocol::{RpcWireRequest, RpcWireResponse};
use crate::registry::{RpcGroup, RpcMethodEntry};
use crate::serialization::RpcSerializationError;

/// Why a call was not run.
pub enum Rejection {
  /// No procedure has this tag (HTTP 404 Problem).
  UnknownProcedure {
    /// The tag the caller sent.
    tag: String,
  },
  /// A guard answered instead of the handler (its own response, typically 401 or 403).
  Refused(Box<Response>),
  /// The payload failed the procedure's input schema.
  InvalidPayload(RpcSerializationError),
}

impl fmt::Debug for Rejection {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::UnknownProcedure { tag } => f
        .debug_struct("UnknownProcedure")
        .field("tag", tag)
        .finish(),
      Self::Refused(response) => f.debug_tuple("Refused").field(&response.status()).finish(),
      Self::InvalidPayload(err) => f.debug_tuple("InvalidPayload").field(err).finish(),
    }
  }
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

/// Look up `call.tag` in `group`, authorize it against `parts`, and run it with `env`.
pub async fn dispatch(
  group: &RpcGroup,
  parts: &Parts,
  call: RpcWireRequest,
  env: DynEnv,
) -> Result<RpcWireResponse, Rejection> {
  let Some(entry) = group.get(&call.tag) else {
    return Err(Rejection::UnknownProcedure { tag: call.tag });
  };
  authorize(entry, parts).await?;
  entry
    .dispatch(call.payload, env)
    .await
    .map_err(Rejection::InvalidPayload)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::registry::{Procedure, RouteGuard};
  use axum::http::{Request, StatusCode};
  use axum::response::IntoResponse;
  use id_effect::{schema, succeed};
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

  fn call(tag: &str) -> RpcWireRequest {
    RpcWireRequest {
      tag: tag.to_owned(),
      payload: serde_json::json!("Ada"),
    }
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
      Arc::new(schema::string::<()>()),
      Arc::new(schema::string::<()>()),
      &["name"],
      |name, _| succeed(name),
    );
    group.register(
      Procedure::query("A.closed")
        .guard(probe("never", false))
        .guard(probe("deny", true)),
      Arc::new(schema::string::<()>()),
      Arc::new(schema::string::<()>()),
      &["name"],
      |name, _| succeed(name),
    );
    let ok = dispatch(&group, &parts(), call("A.open"), DynEnv::new()).await;
    assert!(matches!(ok, Ok(RpcWireResponse::Success { .. })));
    assert_eq!(*order.lock().unwrap(), ["outer", "inner"]);

    let refused = dispatch(&group, &parts(), call("A.closed"), DynEnv::new()).await;
    let Err(Rejection::Refused(response)) = refused else {
      panic!("expected a refusal");
    };
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "the inner guard never ran");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn unknown_tags_and_bad_payloads_are_rejections() {
    let mut group = RpcGroup::new();
    group.register(
      Procedure::query("A.n").public(),
      Arc::new(schema::i64::<()>()),
      Arc::new(schema::string::<()>()),
      &["text"],
      |n, _| succeed(n.to_string()),
    );
    let unknown = dispatch(&group, &parts(), call("A.nope"), DynEnv::new()).await;
    let err = unknown.unwrap_err();
    assert!(format!("{err:?}").contains("A.nope"));
    assert!(matches!(err, Rejection::UnknownProcedure { tag } if tag == "A.nope"));
    let bad = dispatch(&group, &parts(), call("A.n"), DynEnv::new()).await;
    let err = bad.unwrap_err();
    assert!(format!("{err:?}").starts_with("InvalidPayload"));
    let refused = Rejection::Refused(Box::new(StatusCode::UNAUTHORIZED.into_response()));
    assert!(format!("{refused:?}").contains("401"));
  }
}
