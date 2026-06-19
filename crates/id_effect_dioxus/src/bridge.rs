//! SSR bridge: run `Effect` and return HTML for hydration.

use id_effect::{CapBindR, Effect, succeed};
use id_effect_axum::run_with_env;
use serde::{Deserialize, Serialize};

/// Incoming SSR request metadata (path, props JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsrRequest {
  /// Route path, e.g. `/dashboard`.
  pub path: String,
  /// Serialized component props (JSON object).
  pub props_json: String,
}

/// HTML fragment returned to the browser for hydration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsrResponse {
  /// HTML body fragment.
  pub html: String,
}

/// Trait implemented by app-specific bridges (Dioxus app root or test double).
pub trait SsrBridge: Send + Sync + 'static {
  /// Render `req` to HTML. Default stub wraps props in a diagnostic div.
  fn render(&self, req: &SsrRequest) -> String {
    format!(
      "<div data-id-effect-ssr path=\"{}\">{}</div>",
      req.path, req.props_json
    )
  }
}

/// Default bridge used when feature `dioxus` is disabled.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultSsrBridge;

impl SsrBridge for DefaultSsrBridge {}

/// Run `build` as an `Effect`, then render via `bridge`.
pub async fn render_effect<S, A, E, B, F>(
  env: S,
  bridge: B,
  req: SsrRequest,
  build: F,
) -> Result<SsrResponse, E>
where
  S: Send + 'static,
  A: Send + 'static,
  E: Send + 'static,
  B: SsrBridge,
  F: FnOnce(&mut S) -> Effect<A, E, S>,
{
  let _domain = run_with_env(env, build).await?;
  Ok(SsrResponse {
    html: bridge.render(&req),
  })
}

/// Convenience effect that only renders (no extra domain work).
#[inline]
pub fn render_request<S, B>(
  bridge: B,
  req: SsrRequest,
) -> Effect<SsrResponse, std::convert::Infallible, S>
where
  S: CapBindR + 'static,
  B: SsrBridge + Clone + 'static,
{
  let b = bridge.clone();
  succeed(SsrResponse {
    html: b.render(&req),
  })
}
