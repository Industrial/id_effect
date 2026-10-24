//! What a tool sees of the MCP request it runs in: the principal, progress reporting and
//! server-minted handles bound to the principal (ADR 0015 §7).
//!
//! The server runs every request's effect with a fiber-local scope, so a tool handler (an
//! ordinary `Effect`) can call [`report_progress`], [`mint_handle`] and [`resolve_handle`]
//! without new parameters. Outside an MCP request, progress is a no-op and handles fail with
//! [`HandleError::NoRequest`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use id_effect::{Effect, FiberRef, fail, succeed};
use serde_json::{Map, Value};
use thiserror::Error;

/// Delivers one `notifications/progress` params object to the request's peer.
pub(crate) type ProgressSink = Arc<dyn Fn(Value) + Send + Sync>;

/// The fiber-local scope of one MCP request.
pub(crate) struct RequestScope {
  pub(crate) principal: Arc<str>,
  pub(crate) progress: Option<(Value, ProgressSink)>,
  pub(crate) handles: Handles,
}

fn scope_ref() -> &'static FiberRef<Option<Arc<RequestScope>>> {
  static SCOPE: OnceLock<FiberRef<Option<Arc<RequestScope>>>> = OnceLock::new();
  SCOPE.get_or_init(|| FiberRef::new(None))
}

/// Runs `eff` inside `scope`.
pub(crate) fn within<A, E>(scope: RequestScope, eff: Effect<A, E>) -> Effect<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  scope_ref().locally(Some(Arc::new(scope)), eff)
}

/// Sends a `notifications/progress` for the running MCP request, when its client asked for
/// progress (a `progressToken`); otherwise, and outside an MCP request, does nothing. `progress`
/// should increase with every call.
///
/// ```
/// use id_effect::{Effect, succeed};
/// use id_effect_ai::mcp::report_progress;
///
/// let step: Effect<u8, String> = report_progress(1.0, Some(2.0), Some("half way".into()))
///   .flat_map(|()| succeed(1));
/// assert_eq!(step.block_on().into_result().unwrap(), 1);
/// ```
pub fn report_progress<E, R>(
  progress: f64,
  total: Option<f64>,
  message: Option<String>,
) -> Effect<(), E, R>
where
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  scope_ref()
    .get()
    .map(move |scope: Option<Arc<RequestScope>>| {
      if let Some((token, sink)) = scope.as_ref().and_then(|s| s.progress.as_ref()) {
        let mut params = Map::new();
        params.insert("progressToken".into(), token.clone());
        params.insert("progress".into(), Value::from(progress));
        if let Some(total) = total {
          params.insert("total".into(), Value::from(total));
        }
        if let Some(message) = message {
          params.insert("message".into(), Value::String(message));
        }
        sink(Value::Object(params));
      }
    })
}

/// The id of the principal the running MCP request acts for (`None` outside an MCP request).
/// Over stdio it is [`STDIO_PRINCIPAL`](super::STDIO_PRINCIPAL).
pub fn current_principal<E, R>() -> Effect<Option<String>, E, R>
where
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  scope_ref()
    .get()
    .map(|scope: Option<Arc<RequestScope>>| scope.map(|s| s.principal.to_string()))
}

/// Why a handle could not be minted or resolved.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum HandleError {
  /// Not running inside an MCP request.
  #[error("no MCP request is running")]
  NoRequest,
  /// The handle does not exist, expired, or belongs to another principal (the answer is the
  /// same in each case, so a handle's existence does not leak).
  #[error("unknown handle")]
  Unknown,
  /// The server holds its maximum number of handles.
  #[error("too many handles")]
  Full,
}

/// Mints a handle for `value`: a 128-bit random id bound to the running request's principal,
/// for state a later call of the same principal passes back (ADR 0015 §7). Each principal keeps
/// at most [`McpLimits::max_handles_per_principal`](super::McpLimits) (its oldest is evicted)
/// and a handle expires after [`McpLimits::handle_ttl`](super::McpLimits).
pub fn mint_handle<R>(value: Value) -> Effect<String, HandleError, R>
where
  R: Send + Sync + 'static,
{
  scope_ref()
    .get()
    .flat_map(move |scope: Option<Arc<RequestScope>>| match scope {
      None => fail(HandleError::NoRequest),
      Some(s) => match s.handles.mint(&s.principal, value) {
        Ok(id) => succeed(id),
        Err(e) => fail(e),
      },
    })
}

/// The value of `handle`, if the running request's principal minted it.
/// [`HandleError::Unknown`] for a handle of another principal.
pub fn resolve_handle<R>(handle: impl Into<String>) -> Effect<Value, HandleError, R>
where
  R: Send + Sync + 'static,
{
  let handle = handle.into();
  scope_ref()
    .get()
    .flat_map(move |scope: Option<Arc<RequestScope>>| match scope {
      None => fail(HandleError::NoRequest),
      Some(s) => match s.handles.resolve(&s.principal, &handle) {
        Ok(v) => succeed(v),
        Err(e) => fail(e),
      },
    })
}

struct Entry {
  principal: Arc<str>,
  value: Value,
  created: Instant,
  seq: u64,
}

#[derive(Default)]
struct HandleMap {
  entries: HashMap<String, Entry>,
  seq: u64,
}

/// The server's store of minted identifiers (handles and pagination cursors). Clones share it.
#[derive(Clone)]
pub(crate) struct Handles {
  map: Arc<Mutex<HandleMap>>,
  per_principal: usize,
  total: usize,
  ttl: Duration,
}

impl Handles {
  pub(crate) fn new(per_principal: usize, total: usize, ttl: Duration) -> Self {
    Self {
      map: Arc::default(),
      per_principal: per_principal.max(1),
      total: total.max(1),
      ttl,
    }
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, HandleMap> {
    self
      .map
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  pub(crate) fn mint(&self, principal: &Arc<str>, value: Value) -> Result<String, HandleError> {
    let mut map = self.lock();
    let ttl = self.ttl;
    map.entries.retain(|_, e| e.created.elapsed() < ttl);
    let own: Vec<(u64, String)> = map
      .entries
      .iter()
      .filter(|(_, e)| e.principal == *principal)
      .map(|(k, e)| (e.seq, k.clone()))
      .collect();
    if own.len() >= self.per_principal
      && let Some((_, oldest)) = own.into_iter().min()
    {
      map.entries.remove(&oldest);
    }
    if map.entries.len() >= self.total {
      return Err(HandleError::Full);
    }
    let id = format!("{:032x}", rand::random::<u128>());
    map.seq += 1;
    let seq = map.seq;
    map.entries.insert(
      id.clone(),
      Entry {
        principal: Arc::clone(principal),
        value,
        created: Instant::now(),
        seq,
      },
    );
    Ok(id)
  }

  pub(crate) fn resolve(&self, principal: &str, id: &str) -> Result<Value, HandleError> {
    let map = self.lock();
    match map.entries.get(id) {
      Some(e) if &*e.principal == principal && e.created.elapsed() < self.ttl => {
        Ok(e.value.clone())
      }
      _ => Err(HandleError::Unknown),
    }
  }

  #[cfg(test)]
  pub(crate) fn len(&self) -> usize {
    self.lock().entries.len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::HNil;
  use serde_json::json;
  use std::sync::Mutex as StdMutex;

  fn scope(handles: &Handles, sink: Option<ProgressSink>) -> RequestScope {
    RequestScope {
      principal: Arc::from("alice"),
      progress: sink.map(|s| (json!("tok"), s)),
      handles: handles.clone(),
    }
  }

  #[test]
  fn handles_are_random_bound_capped_and_expire() {
    let h = Handles::new(2, 3, Duration::from_secs(60));
    let alice: Arc<str> = Arc::from("alice");
    let bob: Arc<str> = Arc::from("bob");
    let a1 = h.mint(&alice, json!(1)).unwrap();
    assert_eq!(a1.len(), 32);
    assert_eq!(h.resolve("alice", &a1), Ok(json!(1)));
    assert_eq!(h.resolve("bob", &a1), Err(HandleError::Unknown));
    let a2 = h.mint(&alice, json!(2)).unwrap();
    assert_ne!(a1, a2);
    // Alice's third handle evicts her oldest.
    let a3 = h.mint(&alice, json!(3)).unwrap();
    assert_eq!(h.resolve("alice", &a1), Err(HandleError::Unknown));
    assert_eq!(h.resolve("alice", &a3), Ok(json!(3)));
    h.mint(&bob, json!(4)).unwrap();
    assert_eq!(h.len(), 3);
    let carol: Arc<str> = Arc::from("carol");
    assert_eq!(h.mint(&carol, json!(5)), Err(HandleError::Full));

    let expiring = Handles::new(4, 4, Duration::ZERO);
    let id = expiring.mint(&alice, json!(1)).unwrap();
    assert_eq!(expiring.resolve("alice", &id), Err(HandleError::Unknown));
  }

  #[test]
  fn the_scope_reaches_effects_run_within_it() {
    let handles = Handles::new(8, 8, Duration::from_secs(60));
    let seen: Arc<StdMutex<Vec<Value>>> = Arc::default();
    let sink_seen = Arc::clone(&seen);
    let sink: ProgressSink = Arc::new(move |v| sink_seen.lock().unwrap().push(v));
    let eff = report_progress::<HandleError, HNil>(1.0, Some(4.0), Some("m".into()))
      .flat_map(|()| report_progress(2.0, None, None))
      .flat_map(|()| mint_handle(json!({ "cart": 1 })))
      .flat_map(resolve_handle)
      .flat_map(|v| current_principal().map(move |p| (v, p)));
    let (value, principal) = within(scope(&handles, Some(sink)), eff)
      .block_on()
      .into_result()
      .unwrap();
    assert_eq!(value, json!({ "cart": 1 }));
    assert_eq!(principal.as_deref(), Some("alice"));
    let seen = seen.lock().unwrap();
    assert_eq!(
      seen[0],
      json!({ "progressToken": "tok", "progress": 1.0, "total": 4.0, "message": "m" })
    );
    assert_eq!(seen[1], json!({ "progressToken": "tok", "progress": 2.0 }));

    // Without a progress token, progress is a no-op.
    let quiet = within(
      scope(&handles, None),
      report_progress::<(), HNil>(1.0, None, None),
    );
    assert!(quiet.block_on().into_result().is_ok());
  }

  #[test]
  fn outside_a_request_handles_fail_and_progress_is_silent() {
    assert!(
      report_progress::<(), HNil>(1.0, None, None)
        .block_on()
        .into_result()
        .is_ok()
    );
    assert_eq!(
      current_principal::<(), HNil>().block_on().into_result(),
      Ok(None)
    );
    let minted = mint_handle::<HNil>(json!(1)).block_on().into_result();
    assert_eq!(
      minted.unwrap_err().fail_option(),
      Some(HandleError::NoRequest)
    );
    let resolved = resolve_handle::<HNil>("x").block_on().into_result();
    assert_eq!(
      resolved.unwrap_err().fail_option(),
      Some(HandleError::NoRequest)
    );
    let full = Handles::new(1, 1, Duration::from_secs(1));
    full.mint(&Arc::from("a"), json!(1)).unwrap();
    let s = RequestScope {
      principal: Arc::from("b"),
      progress: None,
      handles: full,
    };
    let err = within(s, mint_handle::<HNil>(json!(2)))
      .block_on()
      .into_result();
    assert_eq!(err.unwrap_err().fail_option(), Some(HandleError::Full));
    assert_eq!(HandleError::Full.to_string(), "too many handles");
  }
}
