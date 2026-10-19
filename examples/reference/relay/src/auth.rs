//! Who is calling: bearer JWTs verified by the kit's [`Authenticate`] layer, the guard every
//! client procedure carries ([`user`]), and the principal a handler reads from its call
//! environment ([`principal`]).
//!
//! Browsers cannot set an `Authorization` header on a WebSocket or a WebTransport session, so
//! the two upgrade paths (and only those) also take the token as the `access_token` query
//! parameter ([`token_from_query`]): it is moved into the header before authentication and
//! removed from the URI, so nothing downstream logs it. Use short-lived tokens there.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::Request;
use axum::http::{HeaderValue, Uri, header};
use axum::middleware::Next;
use axum::response::Response;
use id_effect::{Ctx, DynEnv, Effect};
use id_effect_kit::auth::{Authenticate, JwtIssuer, JwtKey, JwtVerifier};
use id_effect_kit::authz::guard::Guard;
use id_effect_kit::authz::{CurrentPrincipal, authenticated};
use id_effect_kit::rpc::channels::ConnectionId;

/// The audience every Relay token names.
pub const AUDIENCE: &str = "relay";

/// The query parameter the upgrade paths take a token from.
pub const TOKEN_PARAM: &str = "access_token";

/// The paths whose upgrades take [`TOKEN_PARAM`]: the RPC WebSocket and WebTransport endpoints.
pub const UPGRADE_PATHS: [&str; 2] = ["/rpc/ws", "/rpc/wt"];

/// The shortest HMAC secret accepted (256 bits, HS256).
pub const MIN_SECRET_BYTES: usize = 32;

/// How tokens are checked: an HS256 secret and the issuer.
#[derive(Clone)]
pub struct TokenKeys {
  secret: Arc<[u8]>,
  issuer: String,
}

impl std::fmt::Debug for TokenKeys {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TokenKeys")
      .field("issuer", &self.issuer)
      .finish_non_exhaustive()
  }
}

impl TokenKeys {
  /// Tokens signed with `secret` (at least [`MIN_SECRET_BYTES`]) by `issuer`.
  ///
  /// # Errors
  ///
  /// When the secret is shorter than [`MIN_SECRET_BYTES`] or the issuer is empty.
  pub fn new(secret: impl Into<Vec<u8>>, issuer: impl Into<String>) -> Result<Self, String> {
    let secret = secret.into();
    let issuer = issuer.into();
    if secret.len() < MIN_SECRET_BYTES {
      return Err(format!(
        "the token secret must be at least {MIN_SECRET_BYTES} bytes"
      ));
    }
    if issuer.trim().is_empty() {
      return Err("the token issuer must not be empty".to_owned());
    }
    Ok(Self {
      secret: secret.into(),
      issuer,
    })
  }

  /// The verifier: HS256 with this secret, this issuer, audience [`AUDIENCE`], `exp` required.
  pub fn verifier(&self) -> JwtVerifier {
    JwtVerifier::new([JwtKey::hmac(&self.secret)])
      .issuer(self.issuer.clone())
      .audience(AUDIENCE)
      .require_exp(true)
  }

  /// An issuer of tokens these keys verify, valid for `ttl` (tests, and a sign-in service).
  pub fn issuer(&self, ttl: Duration) -> JwtIssuer {
    JwtIssuer::hs256(&self.secret)
      .issuer(self.issuer.clone())
      .audience(AUDIENCE)
      .ttl(ttl)
  }

  /// The authentication layer: bearer JWTs only, realm `relay`.
  pub fn layer(&self) -> Authenticate {
    Authenticate::bearer_jwt(self.verifier()).realm(AUDIENCE)
  }
}

/// The guard of every client procedure: a signed-in user (401 otherwise). Mesh calls carry no
/// user, so they are refused too.
pub fn user() -> Guard {
  Guard::new("relay.user", authenticated())
}

/// Moves `?access_token=` into `Authorization: Bearer` on the [`UPGRADE_PATHS`] (when the
/// request carries no `Authorization` of its own) and strips it from the URI. Every other
/// request passes untouched: a token in the query of any other path is ignored.
pub async fn token_from_query(mut request: Request, next: Next) -> Response {
  let path = request.uri().path().to_owned();
  if UPGRADE_PATHS.contains(&path.as_str())
    && !request.headers().contains_key(header::AUTHORIZATION)
  {
    let (token, rest) = split_token(request.uri().query().unwrap_or_default());
    if let Some(token) = token
      && let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}"))
    {
      request.headers_mut().insert(header::AUTHORIZATION, value);
      let rebuilt = if rest.is_empty() {
        path.clone()
      } else {
        format!("{path}?{rest}")
      };
      if let Ok(uri) = rebuilt.parse::<Uri>() {
        *request.uri_mut() = uri;
      }
    }
  }
  next.run(request).await
}

/// The value of [`TOKEN_PARAM`] in `query` (if any) and the query without it.
fn split_token(query: &str) -> (Option<String>, String) {
  let mut token = None;
  let mut rest = Vec::new();
  for pair in query.split('&').filter(|p| !p.is_empty()) {
    match pair.split_once('=') {
      Some((TOKEN_PARAM, value)) if token.is_none() && !value.is_empty() => {
        token = Some(value.to_owned());
      }
      _ => rest.push(pair),
    }
  }
  (token, rest.join("&"))
}

/// The call's principal (anonymous without one): what [`Authenticate`] put in the request's
/// capabilities, which every RPC transport carries into the call environment.
pub fn principal<E: Send + 'static>() -> Effect<CurrentPrincipal, E, DynEnv> {
  Effect::from_ctx(|ctx: Ctx<DynEnv>| async move {
    Ok(
      ctx
        .env()
        .try_get::<CurrentPrincipal>()
        .map_or_else(|_| CurrentPrincipal::anonymous(), |p| (*p).clone()),
    )
  })
}

/// The call's connection id (one per WebSocket, WebTransport session or SSE stream), when the
/// transport has one.
pub fn connection<E: Send + 'static>() -> Effect<Option<String>, E, DynEnv> {
  Effect::from_ctx(|ctx: Ctx<DynEnv>| async move {
    Ok(
      ctx
        .env()
        .try_get::<ConnectionId>()
        .ok()
        .map(|c| c.0.clone()),
    )
  })
}

/// The id the RPC server scopes idempotency keys and resumed sessions by: the authenticated
/// principal's.
pub fn rpc_principal(parts: &axum::http::request::Parts) -> Option<String> {
  parts
    .extensions
    .get::<CurrentPrincipal>()
    .filter(|p| p.is_authenticated())
    .map(|p| p.id().to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_token_parameter_is_split_out() {
    assert_eq!(
      split_token("a=1&access_token=t0k&b=2"),
      (Some("t0k".to_owned()), "a=1&b=2".to_owned())
    );
    assert_eq!(split_token(""), (None, String::new()));
    assert_eq!(
      split_token("access_token=&x"),
      (None, "access_token=&x".to_owned())
    );
  }

  #[test]
  fn weak_keys_are_refused() {
    assert!(TokenKeys::new(vec![0; 8], "iss").is_err());
    assert!(TokenKeys::new(vec![0; 32], " ").is_err());
    let keys = TokenKeys::new(vec![7; 32], "https://id.example").unwrap();
    assert!(format!("{keys:?}").contains("id.example"));
    assert!(!format!("{keys:?}").contains("secret"));
  }
}
