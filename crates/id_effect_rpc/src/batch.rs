//! Batches (design note §2.2): `POST /rpc` with a JSON **array** of [`WireCall`]s runs every
//! call and answers a JSON array of [`WireResult`]s **in request order**, ids echoed, each call
//! failing on its own; a batch is always `200`.
//!
//! - **Finite** (ADR 0013 R4, C2): at most [`MAX_BATCH_CALLS`] (50) calls and [`MAX_BATCH_BYTES`]
//!   (1 MiB) per batch ([`BatchConfig`]); over either is a `413` Problem before any element is
//!   decoded ([`decode_batch`] counts the elements first).
//! - **Kinds**: only queries and mutations are batched. A stream kind, a subscription or a
//!   two-way stream in a batch — and a tag the server does not serve — is that call's
//!   `InvalidInput` failure (issue path `/tag`).
//! - **Access**: every call's guards run, outermost first, before any call runs; a refusal
//!   answers the whole batch with the guard's own response (header [`BATCH_INDEX_HEADER`] names
//!   the refused call), so no call of a batch runs beside a refused one.
//! - **Rate limit**: a batch of N calls costs N tokens. [`RateLimitLayer`] took one for the
//!   request; the batch takes the other N − 1 through its [`RateLimitCharge`], or answers the
//!   layer's `429`.
//! - **Concurrency**: the calls run with bounded parallelism through the server's compute
//!   [`AdmissionController`] (one permit per running call, [`DEFAULT_BATCH_PARALLELISM`] by
//!   default, shared by every batch of the server); with `x-rpc-ordered: true`
//!   ([`ORDERED_HEADER`]) they run one after the other in array order. The request's deadline
//!   (`x-rpc-deadline-ms`, clamped) bounds the whole batch, admission waits included.
//! - **Deduplication**: one [`RequestCache`] per batch, so equal loads through a
//!   `RequestResolver` by several calls of the batch reach the resolver once (the DataLoader
//!   equivalent); nothing is shared between batches.
//! - **Idempotency**: with an `Idempotency-Key`, each mutation of the batch is keyed by
//!   `<key>#<call id>` in the usual `(principal, tag@version, key)` scope (anonymous callers:
//!   client IP, keys of 16 characters or more), so a retried batch replays every mutation that
//!   completed. A key reused with another payload, or still running, is a `409` Problem for the
//!   batch (nothing runs).
//!
//! ```rust
//! use id_effect_rpc::batch::{BatchConfig, BatchError, decode_batch};
//! use id_effect_rpc::wire::{MAX_BATCH_BYTES, MAX_BATCH_CALLS};
//!
//! let body = br#"[{"id":1,"tag":"Users.get","payload":1},{"id":2,"tag":"Users.get","payload":2}]"#;
//! let calls = decode_batch(body, MAX_BATCH_CALLS, MAX_BATCH_BYTES).unwrap();
//! assert_eq!(calls.len(), 2);
//! assert!(matches!(
//!   decode_batch(body, 1, MAX_BATCH_BYTES),
//!   Err(BatchError::TooManyCalls { max: 1 })
//! ));
//! let config = BatchConfig::new().with_max_calls(20).with_parallelism(4);
//! assert_eq!((config.max_calls(), config.parallelism()), (20, 4));
//! ```
//!
//! [`RateLimitLayer`]: id_effect_axum::RateLimitLayer

use std::cell::Cell;
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use id_effect::RequestCache;
use id_effect::compute::AdmissionController;
use id_effect_axum::middleware::rate_limit::RateLimitCharge;
use id_effect_axum::route::Problem;
use id_effect_axum::{ClientIp, TrustedProxies};
use serde::Deserializer as _;
use serde::de::{Error as _, IgnoredAny, SeqAccess, Visitor};
use serde_json::Value;
use tracing::Instrument as _;

use crate::cancel::{CallControl, effective_deadline, requested_deadline};
use crate::contract::ProcedureKind;
use crate::error_union::failure_value;
use crate::handler::{CallContext, CallOutcome};
use crate::idempotency::{
  Begin, IDEMPOTENCY_KEY_HEADER, IdempotencyKey, IdempotencyScope, Reservation, StoredResult,
  parse_key, payload_digest,
};
use crate::registry::RpcMethodEntry;
use crate::server::RpcServer;
use crate::server::dispatch::{self, Rejection};
use crate::wire::{
  Batch, CallId, InputIssue, MAX_BATCH_BYTES, MAX_BATCH_CALLS, RpcFailure, WireCall, WireExit,
  WireResult,
};

/// Request header: `true` runs the calls of a batch one after the other, in array order.
pub const ORDERED_HEADER: &str = "x-rpc-ordered";

/// Response header of a batch refused by a guard: the index of the refused call.
pub const BATCH_INDEX_HEADER: &str = "x-rpc-batch-index";

/// Default number of batch calls a server runs at once (across all its batches).
pub const DEFAULT_BATCH_PARALLELISM: usize = 8;

/// A server's batch limits and parallelism ([`RpcServer::with_batch_config`]).
#[derive(Clone)]
pub struct BatchConfig {
  max_calls: usize,
  max_bytes: usize,
  admission: Arc<AdmissionController>,
}

impl fmt::Debug for BatchConfig {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("BatchConfig")
      .field("max_calls", &self.max_calls)
      .field("max_bytes", &self.max_bytes)
      .field("parallelism", &self.parallelism())
      .finish()
  }
}

impl Default for BatchConfig {
  fn default() -> Self {
    Self::new()
  }
}

impl BatchConfig {
  /// [`MAX_BATCH_CALLS`] calls, [`MAX_BATCH_BYTES`] bytes, [`DEFAULT_BATCH_PARALLELISM`] calls at
  /// once.
  pub fn new() -> Self {
    Self {
      max_calls: MAX_BATCH_CALLS,
      max_bytes: MAX_BATCH_BYTES,
      admission: Arc::new(AdmissionController::new(
        DEFAULT_BATCH_PARALLELISM,
        DEFAULT_BATCH_PARALLELISM,
      )),
    }
  }

  /// At most `calls` (at least 1) calls per batch.
  #[must_use]
  pub fn with_max_calls(mut self, calls: usize) -> Self {
    self.max_calls = calls.max(1);
    self
  }

  /// At most `bytes` of body per batch (the server's body cap applies first).
  #[must_use]
  pub fn with_max_bytes(mut self, bytes: usize) -> Self {
    self.max_bytes = bytes;
    self
  }

  /// Run at most `calls` (at least 1) batch calls at once, through a fresh admission controller.
  #[must_use]
  pub fn with_parallelism(mut self, calls: usize) -> Self {
    let n = calls.max(1);
    self.admission = Arc::new(AdmissionController::new(n, n));
    self
  }

  /// Admit batch calls through `admission` (for example the controller a `FabricGovernor`
  /// rebalances, so batches yield to the host's resource policy).
  #[must_use]
  pub fn with_admission(mut self, admission: Arc<AdmissionController>) -> Self {
    self.admission = admission;
    self
  }

  /// The call cap.
  pub fn max_calls(&self) -> usize {
    self.max_calls
  }

  /// The byte cap.
  pub fn max_bytes(&self) -> usize {
    self.max_bytes
  }

  /// The most calls admitted at once.
  pub fn parallelism(&self) -> usize {
    self.admission.max_permits()
  }

  /// The admission controller.
  pub fn admission(&self) -> &Arc<AdmissionController> {
    &self.admission
  }
}

impl RpcServer {
  /// Batch limits and parallelism (default [`BatchConfig::new`]).
  #[must_use]
  pub fn with_batch_config(mut self, config: BatchConfig) -> Self {
    self.batch = config;
    self
  }

  /// The batch configuration.
  pub fn batch_config(&self) -> &BatchConfig {
    &self.batch
  }
}

/// Why a batch body was refused before any call ran.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
  /// The body is longer than the byte cap (`413`).
  #[error("the rpc batch is {len} bytes, more than {max}")]
  TooLarge {
    /// The body's length.
    len: usize,
    /// The cap.
    max: usize,
  },
  /// More calls than the cap (`413`).
  #[error("an rpc batch carries at most {max} calls")]
  TooManyCalls {
    /// The cap.
    max: usize,
  },
  /// `[]` (`400`).
  #[error("empty rpc batches are refused")]
  Empty,
  /// Not a JSON array of calls (`400`).
  #[error("malformed rpc batch: {0}")]
  Malformed(String),
  /// A call without an `id` (`400`).
  #[error("call {index} of the rpc batch has no id")]
  MissingId {
    /// The call's index.
    index: usize,
  },
  /// Two calls with one `id` (`400`).
  #[error("the rpc batch uses call id {id} twice")]
  DuplicateId {
    /// The id.
    id: u64,
  },
}

impl BatchError {
  /// `413` for a cap, `400` otherwise.
  pub fn status(&self) -> StatusCode {
    match self {
      Self::TooLarge { .. } | Self::TooManyCalls { .. } => StatusCode::PAYLOAD_TOO_LARGE,
      _ => StatusCode::BAD_REQUEST,
    }
  }

  /// The Problem `code` extension.
  pub fn code(&self) -> &'static str {
    match self {
      Self::TooLarge { .. } => "BatchTooLarge",
      Self::TooManyCalls { .. } => "BatchTooManyCalls",
      Self::Empty => "BatchEmpty",
      Self::Malformed(_) => "BatchMalformed",
      Self::MissingId { .. } => "BatchCallIdMissing",
      Self::DuplicateId { .. } => "BatchCallIdDuplicate",
    }
  }
}

/// Counts the elements of a JSON array without decoding them, stopping past `max`.
struct Count<'a> {
  max: usize,
  over: &'a Cell<bool>,
}

impl<'de> Visitor<'de> for Count<'_> {
  type Value = usize;

  fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("a JSON array of rpc calls")
  }

  fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<usize, A::Error> {
    let mut n = 0;
    while seq.next_element::<IgnoredAny>()?.is_some() {
      n += 1;
      if n > self.max {
        self.over.set(true);
        return Err(A::Error::custom("too many calls"));
      }
    }
    Ok(n)
  }
}

/// Decode a batch body: at most `max_bytes` and `max_calls` (both checked before any element is
/// decoded), then every element a [`WireCall`] with an id no other call uses. The fuzz target's
/// entry point.
pub fn decode_batch(bytes: &[u8], max_calls: usize, max_bytes: usize) -> Result<Batch, BatchError> {
  if bytes.len() > max_bytes {
    return Err(BatchError::TooLarge {
      len: bytes.len(),
      max: max_bytes,
    });
  }
  let over = Cell::new(false);
  let mut de = serde_json::Deserializer::from_slice(bytes);
  let counted = de
    .deserialize_seq(Count {
      max: max_calls,
      over: &over,
    })
    .and_then(|n| de.end().map(|()| n));
  let count = match counted {
    Ok(n) => n,
    Err(_) if over.get() => return Err(BatchError::TooManyCalls { max: max_calls }),
    Err(err) => return Err(BatchError::Malformed(err.to_string())),
  };
  if count == 0 {
    return Err(BatchError::Empty);
  }
  let calls: Batch =
    serde_json::from_slice(bytes).map_err(|err| BatchError::Malformed(err.to_string()))?;
  let mut seen = HashSet::with_capacity(calls.len());
  for (index, call) in calls.iter().enumerate() {
    let Some(id) = call.id else {
      return Err(BatchError::MissingId { index });
    };
    if !seen.insert(id) {
      return Err(BatchError::DuplicateId { id: id.get() });
    }
  }
  Ok(calls)
}

/// Whether a request body is a batch: a JSON array.
pub(crate) fn is_batch(bytes: &[u8]) -> bool {
  bytes.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[')
}

fn problem(status: StatusCode, code: &str, detail: impl Into<String>, path: &str) -> Response {
  Problem::new(status)
    .with_detail(detail)
    .with_instance(path)
    .with_extension("code", Value::String(code.to_owned()))
    .into_response()
}

fn tag_issue(message: String) -> CallOutcome {
  CallOutcome::invalid_input(vec![InputIssue {
    path: "/tag".to_owned(),
    message,
  }])
}

/// What one call of the batch does once every call was checked.
enum Plan<'g> {
  /// Answer this outcome without running anything.
  Answer(CallOutcome),
  /// Answer a stored (replayed) exit.
  Replay(WireExit),
  /// Run the procedure (a fresh idempotency reservation completes with its result).
  Run {
    entry: &'g RpcMethodEntry,
    reservation: Option<(Reservation, [u8; 32])>,
  },
}

struct Item<'g> {
  index: usize,
  id: Option<CallId>,
  tag: String,
  payload: Value,
  plan: Plan<'g>,
}

/// The idempotency scope of the request: its principal, else the client IP.
fn scope_of(server: &RpcServer, parts: &Parts) -> Option<IdempotencyScope> {
  if let Some(principal) = server.principal_of(parts) {
    return Some(IdempotencyScope::Principal(principal));
  }
  let none = TrustedProxies::none();
  let trusted = parts.extensions.get::<TrustedProxies>().unwrap_or(&none);
  ClientIp::from_parts(&parts.extensions, &parts.headers, trusted)
    .map(|ClientIp(ip)| IdempotencyScope::Anonymous(ip))
}

/// Reserve the batch's mutations under `raw_key`: replays become [`Plan::Replay`], fresh keys
/// carry their reservation; a mismatch or a running key releases every reservation taken and
/// answers the batch.
async fn reserve(
  server: &RpcServer,
  parts: &Parts,
  raw_key: &[u8],
  cid: &str,
  items: &mut [Item<'_>],
) -> Result<(), Response> {
  let path = server.path();
  let principal = server.principal_of(parts);
  let key = parse_key(raw_key, principal.is_none())
    .map_err(|err| problem(StatusCode::BAD_REQUEST, err.code(), err.to_string(), path))?;
  let Some(scope) = scope_of(server, parts) else {
    let detail = "an anonymous Idempotency-Key needs the client address (serve with connect info)";
    return Err(problem(
      StatusCode::BAD_REQUEST,
      "IdempotencyKeyUnscoped",
      detail,
      path,
    ));
  };
  let store = server.idempotency_store();
  for i in 0..items.len() {
    let item = &mut items[i];
    let Plan::Run { entry, reservation } = &mut item.plan else {
      continue;
    };
    if entry.kind() != ProcedureKind::Mutation {
      continue;
    }
    let scoped_tag = format!("{}@{}", item.tag, entry.version());
    // `serde_json::Value` maps are ordered, so equal payloads digest equally.
    let digest = payload_digest(
      &scoped_tag,
      &serde_json::to_vec(&item.payload).unwrap_or_default(),
    );
    // Every call of a batch has an id (`decode_batch`).
    let call_key = format!("{key}#{}", item.id.map_or(0, CallId::get));
    let scoped = IdempotencyKey::new(scope.clone(), scoped_tag, call_key);
    let refusal = match store.begin(&scoped, digest).await {
      Begin::Fresh => {
        *reservation = Some((Reservation::new(store.clone(), scoped), digest));
        continue;
      }
      Begin::Replay(stored) => {
        item.plan = Plan::Replay(serde_json::from_slice(&stored.body).unwrap_or_else(|_| {
          WireExit::Failure {
            error: failure_value(&RpcFailure::InternalError { id: cid.to_owned() }),
          }
        }));
        continue;
      }
      Begin::Mismatch => (
        "IdempotencyKeyReused",
        "this Idempotency-Key was used with another payload",
      ),
      Begin::InProgress => (
        "IdempotencyKeyInProgress",
        "a call with this Idempotency-Key is still running",
      ),
    };
    for earlier in items.iter_mut() {
      if let Plan::Run { reservation, .. } = &mut earlier.plan
        && let Some((held, _)) = reservation.take()
      {
        held.finish(None).await;
      }
    }
    let mut response = problem(StatusCode::CONFLICT, refusal.0, refusal.1, path);
    response
      .headers_mut()
      .insert(BATCH_INDEX_HEADER, HeaderValue::from(i));
    return Err(response);
  }
  Ok(())
}

/// Serve a batch body (already read under the server's body cap) on `POST {path}`.
pub(crate) async fn serve(
  server: &RpcServer,
  ctx: &CallContext,
  parts: &Parts,
  bytes: &[u8],
) -> Response {
  let path = server.path();
  let config = server.batch_config();
  let calls = match decode_batch(bytes, config.max_calls, config.max_bytes) {
    Ok(calls) => calls,
    Err(err) => return problem(err.status(), err.code(), err.to_string(), path),
  };
  // The rate limit layer took one token for the request; a batch of N costs N.
  if let Some(charge) = parts.extensions.get::<RateLimitCharge>() {
    let extra = u32::try_from(calls.len() - 1).unwrap_or(u32::MAX);
    if charge.take(extra).await.is_err() {
      return charge.too_many(path);
    }
  }
  let mut items = Vec::with_capacity(calls.len());
  for (index, call) in calls.into_iter().enumerate() {
    let WireCall {
      id,
      tag,
      v,
      payload,
    } = call;
    let plan = match dispatch::resolve(server.group(), &tag, v) {
      Ok(entry) if !entry.kind().is_unary() => Plan::Answer(tag_issue(format!(
        "`{tag}` is a {}: only queries and mutations are batched",
        entry.kind()
      ))),
      Ok(entry) => match dispatch::authorize(entry, parts).await {
        Ok(()) => Plan::Run {
          entry,
          reservation: None,
        },
        Err(Rejection::Refused(response)) => {
          let mut response = *response;
          response
            .headers_mut()
            .insert(BATCH_INDEX_HEADER, HeaderValue::from(index));
          return response;
        }
        Err(other) => Plan::Answer(tag_issue(format!("{other:?}"))),
      },
      Err(_) => Plan::Answer(tag_issue(match v {
        Some(v) => format!("unknown rpc procedure `{tag}` at version {v}"),
        None => format!("unknown rpc procedure `{tag}`"),
      })),
    };
    items.push(Item {
      index,
      id,
      tag,
      payload,
      plan,
    });
  }
  if let Some(raw) = parts.headers.get(IDEMPOTENCY_KEY_HEADER)
    && let Err(response) = reserve(
      server,
      parts,
      raw.as_bytes(),
      &ctx.correlation_id,
      &mut items,
    )
    .await
  {
    return response;
  }
  let ctx = ctx.clone().with_request_cache(RequestCache::new());
  let deadline = tokio::time::Instant::now()
    + effective_deadline(requested_deadline(&parts.headers), server.max_deadline());
  let admission = config.admission();
  let run = |item| run_call(item, &ctx, admission, deadline);
  let ordered = parts
    .headers
    .get(ORDERED_HEADER)
    .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"true"));
  let results: Vec<WireResult> = if ordered {
    let mut results = Vec::with_capacity(items.len());
    for item in items {
      results.push(run(item).await);
    }
    results
  } else {
    futures_util::future::join_all(items.into_iter().map(run)).await
  };
  // `WireResult`s of JSON values always encode.
  let body = serde_json::to_vec(&results).unwrap_or_default();
  (
    StatusCode::OK,
    [(header::CONTENT_TYPE, "application/json")],
    body,
  )
    .into_response()
}

/// Run one call of a batch under the batch's deadline, admitted through `admission`.
async fn run_call(
  item: Item<'_>,
  ctx: &CallContext,
  admission: &Arc<AdmissionController>,
  deadline: tokio::time::Instant,
) -> WireResult {
  let Item {
    index,
    id,
    tag,
    payload,
    plan,
  } = item;
  let span = tracing::info_span!(
    "rpc.call",
    rpc.operation = %tag,
    rpc.call_id = id.map_or(0, CallId::get),
    rpc.batch_index = index,
  );
  let exit = match plan {
    Plan::Answer(outcome) => outcome.exit,
    Plan::Replay(exit) => exit,
    Plan::Run { entry, reservation } => {
      let run = async {
        let Ok(permit) = tokio::time::timeout_at(deadline, admission.admit()).await else {
          return CallOutcome::failure(RpcFailure::DeadlineExceeded);
        };
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let control = CallControl::none().with_deadline(left);
        let outcome = match dispatch::start_unary(entry, payload, ctx.clone(), control) {
          Ok(call) => call.await,
          // The kind was checked: only queries and mutations reach here.
          Err(_) => CallOutcome::internal(&ctx.correlation_id),
        };
        drop(permit);
        outcome
      };
      let outcome = run.instrument(span.clone()).await;
      if let Some((reservation, digest)) = reservation {
        let stored = outcome.is_replayable().then(|| StoredResult {
          digest,
          status: outcome.status.as_u16(),
          body: serde_json::to_vec(&outcome.exit).unwrap_or_default(),
        });
        reservation.finish(stored).await;
      }
      outcome.exit
    }
  };
  WireResult::new(id, tag, exit)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn decode_checks_caps_before_elements_and_ids_after() {
    let ok = br#" [{"id":1,"tag":"A.b","payload":null},{"id":2,"tag":"A.b","payload":1}] "#;
    assert_eq!(decode_batch(ok, 2, 1024).unwrap().len(), 2);
    assert_eq!(
      decode_batch(ok, 2, 10),
      Err(BatchError::TooLarge {
        len: ok.len(),
        max: 10
      })
    );
    // Over the call cap with elements that are not calls at all: refused before decoding them.
    let many = b"[1,2,3]";
    assert_eq!(
      decode_batch(many, 2, 1024),
      Err(BatchError::TooManyCalls { max: 2 })
    );
    assert_eq!(decode_batch(b" [ ]", 2, 1024), Err(BatchError::Empty));
    for bad in [&b"[1"[..], b"{}", b"[1] x", b"[1]"] {
      assert!(matches!(
        decode_batch(bad, 2, 1024),
        Err(BatchError::Malformed(_))
      ));
    }
    let no_id = br#"[{"id":1,"tag":"A.b","payload":1},{"tag":"A.b","payload":1}]"#;
    assert_eq!(
      decode_batch(no_id, 5, 1024),
      Err(BatchError::MissingId { index: 1 })
    );
    let twice = br#"[{"id":7,"tag":"A.b","payload":1},{"id":7,"tag":"A.c","payload":1}]"#;
    assert_eq!(
      decode_batch(twice, 5, 1024),
      Err(BatchError::DuplicateId { id: 7 })
    );
  }

  #[test]
  fn errors_have_statuses_and_codes() {
    let cases = [
      (
        BatchError::TooLarge { len: 2, max: 1 },
        413,
        "BatchTooLarge",
      ),
      (
        BatchError::TooManyCalls { max: 1 },
        413,
        "BatchTooManyCalls",
      ),
      (BatchError::Empty, 400, "BatchEmpty"),
      (BatchError::Malformed("x".into()), 400, "BatchMalformed"),
      (
        BatchError::MissingId { index: 0 },
        400,
        "BatchCallIdMissing",
      ),
      (
        BatchError::DuplicateId { id: 1 },
        400,
        "BatchCallIdDuplicate",
      ),
    ];
    for (err, status, code) in cases {
      assert_eq!(err.status().as_u16(), status);
      assert_eq!(err.code(), code);
      assert!(!err.to_string().is_empty());
    }
  }

  #[test]
  fn config_builders_clamp_and_report() {
    let config = BatchConfig::default()
      .with_max_calls(0)
      .with_max_bytes(10)
      .with_parallelism(0);
    assert_eq!(config.max_calls(), 1);
    assert_eq!(config.max_bytes(), 10);
    assert_eq!(config.parallelism(), 1);
    let shared = Arc::new(AdmissionController::new(3, 3));
    let config = config.with_admission(shared.clone());
    assert!(Arc::ptr_eq(config.admission(), &shared));
    assert!(format!("{config:?}").contains("parallelism: 3"));
    assert!(is_batch(b"\n [1]") && !is_batch(b"{}") && !is_batch(b""));
  }
}
