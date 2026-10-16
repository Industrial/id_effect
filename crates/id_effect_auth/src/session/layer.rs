//! [`SessionLayer`]: loads the request's session into `id_effect_axum`'s [`RequestCaps`].

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use http::Request;
use http::header::COOKIE;
use id_effect::{Exit, HNil};
use id_effect_axum::RequestCaps;
use tower_layer::Layer;
use tower_service::Service;

use super::{Session, Sessions};

/// Tower layer that reads the session cookie, [`load`](Sessions::load)s the session (enforcing
/// its timeouts) and, when it is live, records the [`Session`] as a request capability
/// ([`RequestCaps`], read with `env.try_get::<Session>()` after
/// [`execute_with_request_caps`](id_effect_axum::execute_with_request_caps)) and as a request
/// extension (axum's `Extension<Session>`).
///
/// A request without a valid cookie, with an expired session, or arriving while the store fails
/// continues with no session: it is unauthenticated, never wrongly authenticated.
#[derive(Clone)]
pub struct SessionLayer {
  sessions: Sessions,
}

impl SessionLayer {
  /// A layer over `sessions`.
  #[inline]
  pub fn new(sessions: Sessions) -> Self {
    Self { sessions }
  }
}

impl fmt::Debug for SessionLayer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SessionLayer")
      .field("cookie", &self.sessions.config().cookie.name)
      .finish()
  }
}

impl<Svc> Layer<Svc> for SessionLayer {
  type Service = SessionService<Svc>;

  fn layer(&self, inner: Svc) -> Self::Service {
    SessionService {
      inner,
      sessions: self.sessions.clone(),
    }
  }
}

/// Service produced by [`SessionLayer`].
#[derive(Clone)]
pub struct SessionService<Svc> {
  inner: Svc,
  sessions: Sessions,
}

impl<Svc> fmt::Debug for SessionService<Svc> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SessionService").finish_non_exhaustive()
  }
}

/// The live session named by the request's cookies, if any.
async fn live_session(sessions: Sessions, cookies: Vec<String>) -> Option<Session> {
  let id = cookies.iter().find_map(|h| sessions.session_id(h))?;
  match id_effect_axum::run_with_env(HNil, move |_| sessions.load(&id)).await {
    Exit::Success(found) => found,
    Exit::Failure(_) => None,
  }
}

impl<Svc, B> Service<Request<B>> for SessionService<Svc>
where
  Svc: Service<Request<B>> + Clone + Send + 'static,
  Svc::Future: Send + 'static,
  B: Send + 'static,
{
  type Response = Svc::Response;
  type Error = Svc::Error;
  type Future = Pin<Box<dyn Future<Output = Result<Svc::Response, Svc::Error>> + Send>>;

  #[inline]
  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, req: Request<B>) -> Self::Future {
    // The clone that was polled ready serves this request (the usual Tower hand-off).
    let ready = self.inner.clone();
    let mut inner = std::mem::replace(&mut self.inner, ready);
    let sessions = self.sessions.clone();
    let (mut parts, body) = req.into_parts();
    let cookies: Vec<String> = parts
      .headers
      .get_all(COOKIE)
      .iter()
      .filter_map(|v| v.to_str().ok().map(str::to_owned))
      .collect();
    Box::pin(async move {
      if let Some(session) = live_session(sessions, cookies).await {
        let mut caps = parts.extensions.remove::<RequestCaps>().unwrap_or_default();
        caps.insert(session.clone());
        parts.extensions.insert(caps);
        parts.extensions.insert(session);
      }
      inner.call(Request::from_parts(parts, body)).await
    })
  }
}

#[cfg(test)]
mod tests {
  use axum::Router;
  use axum::body::Body;
  use axum::extract::State;
  use axum::http::StatusCode;
  use axum::routing::get;
  use id_effect::{DynEnv, Effect, succeed};
  use id_effect_axum::execute_with_request_caps;
  use std::convert::Infallible;
  use tower::ServiceExt;

  use super::super::{MemorySessionStore, SessionConfig};
  use super::*;

  fn whoami(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
    let who = env
      .try_get::<Session>()
      .map_or_else(|_| "anonymous".to_owned(), |s| s.user_id().to_owned());
    succeed(who)
  }

  fn app(sessions: Sessions) -> Router {
    Router::new()
      .route(
        "/",
        get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)),
      )
      .layer(SessionLayer::new(sessions))
      .with_state(DynEnv::new())
  }

  async fn get_body(app: Router, cookie: Option<String>) -> String {
    let mut req = Request::builder().uri("/");
    if let Some(c) = cookie {
      req = req.header(COOKIE, c);
    }
    let res = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
  }

  fn cookie_for(sessions: &Sessions, session: &Session) -> String {
    format!(
      "{}={}",
      sessions.config().cookie.name,
      session.id().as_str()
    )
  }

  #[tokio::test]
  async fn a_live_session_becomes_a_request_capability() {
    let store = MemorySessionStore::new();
    let sessions = Sessions::new(store, SessionConfig::default());
    let s = id_effect_axum::run_with_env(HNil, {
      let sessions = sessions.clone();
      move |_| sessions.login("u1", None)
    })
    .await;
    let Exit::Success(s) = s else {
      panic!("login failed")
    };
    let cookie = format!("theme=dark; {}", cookie_for(&sessions, &s));
    assert_eq!(get_body(app(sessions.clone()), Some(cookie)).await, "u1");
  }

  #[tokio::test]
  async fn no_cookie_unknown_id_or_outage_is_anonymous() {
    let store = MemorySessionStore::new();
    let sessions = Sessions::new(store.clone(), SessionConfig::default());
    assert_eq!(get_body(app(sessions.clone()), None).await, "anonymous");
    let unknown = format!("{}={}", sessions.config().cookie.name, "ab".repeat(32));
    assert_eq!(
      get_body(app(sessions.clone()), Some(unknown.clone())).await,
      "anonymous"
    );
    store.fail_with(Some("down"));
    assert_eq!(
      get_body(app(sessions.clone()), Some(unknown)).await,
      "anonymous"
    );
    assert!(format!("{:?}", SessionLayer::new(sessions)).contains("__Host-id_session"));
  }
}
