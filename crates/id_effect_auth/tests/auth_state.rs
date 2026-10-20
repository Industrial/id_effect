//! 0.11 security state (E11.2, r11-sec-auth-state): the TOTP replay store and the login
//! throttle shared by every instance through PostgreSQL (M18, H5), and rotation at login of the
//! session id and the CSRF token (L5).
//!
//! The `m18_*` and `pg_*` tests need a database at `DATABASE_URL`; two clients with their own
//! pools stand for two instances of a service. When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**. Without it, those tests print an explicit `SKIP` line to stderr and return.
//!
//! Effects run on `id_effect_tokio::TokioRuntime` (sqlx needs tokio's IO driver).

use id_effect::{Cause, Effect, Exit};
use id_effect_auth::{MemorySessionStore, SessionConfig, Sessions};
use id_effect_tokio::TokioRuntime;

async fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(
  eff: Effect<A, E>,
) -> Result<A, E> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

// ---- L5: rotation at login ------------------------------------------------------------------

#[tokio::test]
async fn l5_session_id_rotates_at_login() {
  let store = MemorySessionStore::new();
  let sessions = Sessions::new(store.clone(), SessionConfig::default());
  // An id the client held before logging in (planted by an attacker, or an anonymous session).
  let planted = run(sessions.login("anonymous", None)).await.unwrap();
  let logged_in = run(sessions.login("alice", Some(planted.id())))
    .await
    .unwrap();
  assert_ne!(logged_in.id(), planted.id());
  assert_eq!(logged_in.user_id(), "alice");
  assert_eq!(run(sessions.load(planted.id())).await, Ok(None));
  assert_eq!(store.len(), 1, "the presented session is gone");
  let cookie = sessions.set_cookie(&logged_in);
  assert!(cookie.contains(logged_in.id().as_str()));
  assert!(!cookie.contains(planted.id().as_str()));
  // Every login issues a fresh id, presented or not.
  let again = run(sessions.login("alice", None)).await.unwrap();
  assert_ne!(again.id(), logged_in.id());
}

#[cfg(feature = "axum")]
mod csrf {
  use axum::Router;
  use axum::body::Body;
  use axum::http::{HeaderMap, Request, StatusCode, header};
  use axum::response::{IntoResponse, Response};
  use axum::routing::{get, post};
  use id_effect_axum::server::security::{
    CSRF_COOKIE, CSRF_HEADER, CookieKey, CsrfToken, SecurityLayer,
  };
  use tower::ServiceExt;

  use super::*;

  /// `GET /` renders the CSRF token, `POST /` accepts, `POST /login` logs `alice` in through
  /// `Sessions::login_with_csrf`, presenting the request's session cookie.
  fn app(sessions: Sessions) -> Router {
    Router::new()
      .route(
        "/",
        get(|token: CsrfToken| async move { token.as_str().to_owned() }).post(|| async { "ok" }),
      )
      .route(
        "/login",
        post(move |headers: HeaderMap, token: CsrfToken| {
          let sessions = sessions.clone();
          async move {
            let presented = headers
              .get(header::COOKIE)
              .and_then(|v| v.to_str().ok())
              .and_then(|c| sessions.session_id(c));
            match run(sessions.login_with_csrf("alice", presented.as_ref(), &token)).await {
              Ok((session, fresh)) => (
                [(header::SET_COOKIE, sessions.set_cookie(&session))],
                fresh,
                "logged in",
              )
                .into_response(),
              Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
          }
        }),
      )
      .layer(SecurityLayer::recommended(
        CookieKey::from_bytes(&[7; 64]).expect("64-byte key"),
      ))
  }

  async fn send(app: &Router, request: Request<Body>) -> Response {
    app.clone().oneshot(request).await.unwrap()
  }

  /// The `name=value` pair of the `Set-Cookie` header for `name`, if one was sent.
  fn cookie_pair(response: &Response, name: &str) -> Option<String> {
    response
      .headers()
      .get_all(header::SET_COOKIE)
      .iter()
      .filter_map(|v| v.to_str().ok())
      .find(|v| v.starts_with(&format!("{name}=")))
      .map(|v| v.split(';').next().unwrap().to_owned())
  }

  fn post_with(uri: &str, cookie: &str, token: &str) -> Request<Body> {
    Request::builder()
      .method("POST")
      .uri(uri)
      .header(header::COOKIE, cookie)
      .header(CSRF_HEADER, token)
      .body(Body::empty())
      .unwrap()
  }

  #[tokio::test]
  async fn l5_csrf_token_rotates_at_login() {
    let store = MemorySessionStore::new();
    let sessions = Sessions::new(store.clone(), SessionConfig::default());
    let app = app(sessions.clone());
    let session_cookie = sessions.config().cookie.name.clone();

    // Before login: a CSRF secret, its rendered token, and a planted session id.
    let res = send(&app, Request::get("/").body(Body::empty()).unwrap()).await;
    let old_csrf = cookie_pair(&res, CSRF_COOKIE).expect("csrf cookie issued");
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 16)
      .await
      .unwrap();
    let old_token = String::from_utf8(bytes.to_vec()).unwrap();
    let planted = run(sessions.login("anonymous", None)).await.unwrap();
    let before = format!("{old_csrf}; {session_cookie}={}", planted.id().as_str());

    let login = send(&app, post_with("/login", &before, &old_token)).await;
    assert_eq!(login.status(), StatusCode::OK);
    let new_csrf = cookie_pair(&login, CSRF_COOKIE).expect("login sets a new CSRF cookie");
    assert_ne!(new_csrf, old_csrf);
    let new_token = login.headers()[CSRF_HEADER].to_str().unwrap().to_owned();
    let new_session = cookie_pair(&login, &session_cookie).expect("session cookie");
    assert_ne!(
      new_session,
      format!("{session_cookie}={}", planted.id().as_str()),
      "the session id rotated too"
    );
    assert_eq!(run(sessions.load(planted.id())).await, Ok(None));

    // After login the browser holds the new cookies: the pre-login token no longer validates.
    let after = format!("{new_csrf}; {new_session}");
    let res = send(&app, post_with("/", &after, &old_token)).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = send(&app, post_with("/", &after, &new_token)).await;
    assert_eq!(res.status(), StatusCode::OK);

    // A failed login rotates nothing.
    store.fail_with(Some("down"));
    let failed = send(&app, post_with("/login", &after, &new_token)).await;
    assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(cookie_pair(&failed, CSRF_COOKIE), None);
    store.fail_with(None);
    let res = send(&app, post_with("/", &after, &new_token)).await;
    assert_eq!(res.status(), StatusCode::OK);
  }
}

// ---- M18, H5: state shared by every instance ------------------------------------------------

#[cfg(feature = "pg")]
mod pg {
  use std::net::{IpAddr, Ipv6Addr};
  use std::time::{Duration, Instant};

  use id_effect::{UtcDateTime, WallClock};
  use id_effect_auth::{AuthError, LoginThrottle, PgThrottleStore, ThrottleStore};
  use id_effect_sql::SqlClient;
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};

  use super::*;

  fn pg_required() -> bool {
    std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
  }

  /// Fail (when `ID_EFFECT_REQUIRE_PG=1`) or loudly skip a live test that has no database.
  fn unavailable<T>(test: &str, why: &str) -> Option<T> {
    assert!(
      !pg_required(),
      "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
    );
    eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
    None
  }

  /// One "instance": a connected client with its own pool, or `None` after [`unavailable`]
  /// decided to skip.
  pub(super) async fn instance(test: &str) -> Option<PgSqlClient> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(4)).await {
      Ok(p) => p,
      Err(e) => return unavailable(test, &format!("cannot build pool: {}", e.0)),
    };
    let client = PgSqlClient::new(pool);
    if let Err(e) = run(client.connect()).await {
      return unavailable(test, &format!("cannot connect: {e:?}"));
    }
    Some(client)
  }

  /// Two instances over one database, with the migration 0003 tables in place.
  pub(super) async fn two_instances(test: &str) -> Option<(PgSqlClient, PgSqlClient)> {
    let a = instance(test).await?;
    let b = instance(test).await?;
    // Tests in other processes may create the tables at the same moment; `CREATE … IF NOT
    // EXISTS` can then lose the race once, so retry.
    let store = PgThrottleStore::new(a.clone());
    let mut tries = 0;
    while let Err(e) = run(store.ensure_schema()).await {
      tries += 1;
      assert!(tries < 5, "{test}: ensure_schema: {e}");
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Some((a, b))
  }

  /// A name no other run uses, so tests and reruns never share rows.
  pub(super) fn unique(prefix: &str) -> String {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).expect("random");
    format!("{prefix}-{}", u64::from_le_bytes(bytes))
  }

  pub(super) fn clock() -> WallClock {
    WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_800_000_000_000))
  }

  #[tokio::test]
  async fn pg_login_throttle_shared_across_instances() {
    let test = "pg_login_throttle_shared_across_instances";
    let Some((a, b)) = two_instances(test).await else {
      return;
    };
    let clock = clock();
    let throttle = |client: &PgSqlClient| {
      LoginThrottle::new(PgThrottleStore::new(client.clone()))
        .free_attempts(2)
        .base_delay(Duration::from_secs(60))
        .with_clock(clock.clone())
    };
    let (on_a, on_b) = (throttle(&a), throttle(&b));
    let account = unique("throttle");
    let mut ip = [0_u8; 16];
    getrandom::fill(&mut ip).expect("random");
    let address = IpAddr::V6(Ipv6Addr::from(ip));
    let ip = Some(address);

    // One failure on each instance: with per-process counts neither would lock.
    run(on_a.check(&account, ip)).await.unwrap();
    run(on_a.record_failure(&account, ip)).await.unwrap();
    run(on_b.check(&account, ip)).await.unwrap();
    run(on_b.record_failure(&account, ip)).await.unwrap();
    for t in [&on_a, &on_b] {
      assert!(matches!(
        run(t.check(&account, None)).await,
        Err(AuthError::TooManyAttempts { .. })
      ));
    }
    let other = unique("other");
    assert!(
      run(on_a.check(&other, ip)).await.is_err(),
      "the address is locked on every instance"
    );

    // Success on B clears the account for A too; the address stays locked.
    run(on_b.record_success(&account)).await.unwrap();
    run(on_a.check(&account, None)).await.unwrap();
    assert!(run(on_a.check(&account, ip)).await.is_err());

    // Concurrent failures from both instances are each counted (one upsert per failure).
    let key = format!("account:{account}");
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..10 {
      let store = PgThrottleStore::new(if i % 2 == 0 { a.clone() } else { b.clone() });
      let (key, now) = (key.clone(), UtcDateTime::unsafe_make(1_800_000_000_000));
      tasks.spawn(
        async move { run(store.record_failure(&key, now, Duration::from_secs(3600))).await },
      );
    }
    while let Some(done) = tasks.join_next().await {
      done.expect("task").unwrap();
    }
    let store = PgThrottleStore::new(a.clone());
    let counted = run(store.get(&key)).await.unwrap().expect("counted");
    assert_eq!(counted.count, 10);

    // Leave nothing behind.
    run(store.clear(&key)).await.unwrap();
    run(store.clear(&format!("ip:{address}"))).await.unwrap();
    assert_eq!(run(store.get(&key)).await, Ok(None));
  }

  #[cfg(feature = "totp")]
  mod totp {
    use id_effect_auth::totp::{
      PgTotpStepStore, Totp, TotpConfig, TotpStepStore, TotpVerification,
    };

    use super::*;

    #[tokio::test]
    async fn m18_pg_totp_replay_rejected_across_instances() {
      let test = "m18_pg_totp_replay_rejected_across_instances";
      let Some((a, b)) = two_instances(test).await else {
        return;
      };
      let clock = clock();
      let totp = |client: &PgSqlClient| {
        Totp::new(
          TotpConfig::new("Acme"),
          PgTotpStepStore::new(client.clone()),
        )
        .with_clock(clock.clone())
      };
      let (on_a, on_b) = (totp(&a), totp(&b));
      let user = unique("totp");
      let secret = on_a.enrol("alice@example.com").unwrap().secret().clone();
      let now = UtcDateTime::unsafe_make(1_800_000_000_000);
      let code = on_a.code_at(&secret, now).unwrap();

      assert_eq!(
        run(on_a.verify(&user, &secret, &code)).await,
        Ok(TotpVerification::Valid)
      );
      // The same code on another instance is a replay, not a second login.
      assert_eq!(
        run(on_b.verify(&user, &secret, &code)).await,
        Ok(TotpVerification::Replayed)
      );
      assert_eq!(
        run(on_a.verify(&user, &secret, &code)).await,
        Ok(TotpVerification::Replayed)
      );

      // The next step's code is accepted once, wherever it is presented first.
      clock.jump(Duration::from_secs(30));
      let next = on_b
        .code_at(&secret, UtcDateTime::unsafe_make(1_800_000_030_000))
        .unwrap();
      assert_eq!(
        run(on_b.verify(&user, &secret, &next)).await,
        Ok(TotpVerification::Valid)
      );
      assert_eq!(
        run(on_a.verify(&user, &secret, &next)).await,
        Ok(TotpVerification::Replayed)
      );
      // An earlier step never becomes valid again.
      assert_eq!(
        run(on_a.verify(&user, &secret, &code)).await,
        Ok(TotpVerification::Replayed)
      );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn m18_pg_totp_concurrent_consume_one_wins() {
      let test = "m18_pg_totp_concurrent_consume_one_wins";
      let Some((a, b)) = two_instances(test).await else {
        return;
      };
      let user = unique("race");
      for round in 0..3_u64 {
        let step = 60_000_000 + round;
        let mut tasks = tokio::task::JoinSet::new();
        for i in 0..16 {
          let store = PgTotpStepStore::new(if i % 2 == 0 { a.clone() } else { b.clone() });
          let user = user.clone();
          tasks.spawn(async move { run(store.consume(&user, step)).await });
        }
        let mut winners = 0;
        while let Some(done) = tasks.join_next().await {
          if done.expect("task").unwrap() {
            winners += 1;
          }
        }
        assert_eq!(
          winners, 1,
          "round {round}: exactly one consume of step {step} wins"
        );
      }
      let store = PgTotpStepStore::new(a.clone());
      assert_eq!(run(store.consume(&user, 60_000_002)).await, Ok(false));
      assert_eq!(run(store.consume(&user, 60_000_001)).await, Ok(false));
      assert_eq!(run(store.consume(&user, 60_000_003)).await, Ok(true));
    }
  }
}
