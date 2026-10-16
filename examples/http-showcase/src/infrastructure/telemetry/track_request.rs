//! [`track_request`]: the axum middleware installed by
//! [`Telemetry::instrument`](super::Telemetry::instrument).

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use id_effect::from_async;
use id_effect_axum::{EffectExecutor, exit_response};

use super::{Telemetry, remote_parent};

/// Runs the rest of the stack as one request fiber under the span and timer of
/// [`Telemetry::request_effect`]: the route is the matched template (or
/// [`Telemetry::UNMATCHED`]); the remote parent is only extracted when spans are exported.
pub(crate) async fn track_request(
  State(telemetry): State<Telemetry>,
  request: Request,
  next: Next,
) -> Response {
  let route = request.extensions().get::<MatchedPath>().map_or_else(
    || Telemetry::UNMATCHED.to_owned(),
    |p| p.as_str().to_owned(),
  );
  let method = request.method().as_str().to_owned();
  let parent = telemetry.exports_spans().then(|| remote_parent(&request));
  let inner = from_async(move || async move { Ok(next.run(request).await) });
  let eff = telemetry.request_effect(method, route, parent, inner);
  exit_response(EffectExecutor::global().run_effect(eff).await)
}
