//! JWKS fetch, cache and key rotation against a mocked identity provider (sec.authn #4).
#![cfg(feature = "jwks")]

mod common;

use std::fmt;
use std::time::Duration;

use common::{ISSUER, ec_issuer, rsa_issuer};
use id_effect::{Cause, Effect, Exit};
use id_effect_auth::{AuthError, AuthPrincipal, JwksClient, JwtIssuer, JwtVerifier};
use id_effect_authz::Principal;
use id_effect_tokio::TokioRuntime;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Runs `eff` on the ambient tokio runtime (reqwest needs its IO driver).
async fn run<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + fmt::Debug + 'static,
{
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// Serves a JWKS document with the public keys of `issuers` (replacing what was served).
async fn publish(server: &MockServer, issuers: &[&JwtIssuer]) {
  server.reset().await;
  let keys: Vec<_> = issuers.iter().map(|i| i.jwk().expect("jwk")).collect();
  Mock::given(method("GET"))
    .and(path("/certs"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "keys": keys })))
    .mount(server)
    .await;
}

async fn outage(server: &MockServer) {
  server.reset().await;
  Mock::given(method("GET"))
    .and(path("/certs"))
    .respond_with(ResponseTemplate::new(500))
    .mount(server)
    .await;
}

fn client(server: &MockServer) -> JwksClient {
  JwksClient::new(format!("{}/certs", server.uri())).min_refresh_interval(Duration::ZERO)
}

async fn token(issuer: &JwtIssuer) -> String {
  run(issuer.issue(&AuthPrincipal::new("alice").with_role("viewer")))
    .await
    .expect("token")
}

#[tokio::test]
async fn rotated_kid_is_accepted_after_refresh() {
  let server = MockServer::start().await;
  let old = rsa_issuer("2026-01");
  let new = ec_issuer("2026-07");
  publish(&server, &[&old]).await;
  let jwks = client(&server);
  let verifier = JwtVerifier::jwks(jwks.clone()).issuer(ISSUER);

  let p = run(verifier.verify(&token(&old).await)).await.unwrap();
  assert_eq!((p.id(), p.roles()), ("alice", &["viewer".to_owned()][..]));
  assert_eq!(jwks.fetch_count(), 1);
  run(verifier.verify(&token(&old).await)).await.unwrap();
  assert_eq!(
    jwks.fetch_count(),
    1,
    "known kids are served from the cache"
  );

  // The provider rotates: the new key is published, tokens are signed with it.
  publish(&server, &[&old, &new]).await;
  let p = run(verifier.verify(&token(&new).await)).await.unwrap();
  assert_eq!(p.id(), "alice");
  assert_eq!(jwks.fetch_count(), 2, "an unknown kid triggers one refetch");
  assert_eq!(jwks.cached_keys().len(), 2);

  // The old key is retired: after the next refresh its tokens no longer verify.
  publish(&server, &[&new]).await;
  assert_eq!(run(jwks.refresh()).await, Ok(1));
  assert!(matches!(
    run(verifier.verify(&token(&old).await)).await,
    Err(AuthError::UnknownKey(_))
  ));
}

#[tokio::test]
async fn unknown_kids_are_rate_limited() {
  let server = MockServer::start().await;
  let known = rsa_issuer("k1");
  publish(&server, &[&known]).await;
  let jwks = client(&server).min_refresh_interval(Duration::from_secs(3600));
  let verifier = JwtVerifier::jwks(jwks.clone()).issuer(ISSUER);
  run(verifier.verify(&token(&known).await)).await.unwrap();
  let forged = token(&rsa_issuer("made-up")).await;
  for _ in 0..5 {
    assert!(matches!(
      run(verifier.verify(&forged)).await,
      Err(AuthError::UnknownKey(m)) if m.contains("made-up")
    ));
  }
  assert_eq!(
    jwks.fetch_count(),
    1,
    "made-up kids do not hammer the provider"
  );
}

#[tokio::test]
async fn concurrent_misses_share_one_fetch() {
  let server = MockServer::start().await;
  let issuer = ec_issuer("k1");
  publish(&server, &[&issuer]).await;
  let jwks = client(&server);
  let verifier = JwtVerifier::jwks(jwks.clone()).issuer(ISSUER);
  let token = token(&issuer).await;
  let checks = (0..8).map(|_| {
    let (v, t) = (verifier.clone(), token.clone());
    tokio::spawn(async move { run(v.verify(&t)).await })
  });
  for check in checks.collect::<Vec<_>>() {
    assert!(check.await.unwrap().is_ok());
  }
  assert_eq!(jwks.fetch_count(), 1);
}

#[tokio::test]
async fn an_unreachable_provider_is_unavailable_until_it_recovers() {
  let server = MockServer::start().await;
  let issuer = rsa_issuer("k1");
  outage(&server).await;
  let verifier = JwtVerifier::jwks(client(&server)).issuer(ISSUER);
  let t = token(&issuer).await;
  let err = run(verifier.verify(&t)).await.unwrap_err();
  assert!(
    matches!(&err, AuthError::Unavailable(m) if m.contains("500")),
    "{err:?}"
  );
  assert_eq!(err.http_status(), 503);

  // Rate-limited while the last fetch failed: still unavailable, not "unknown key".
  let limited = JwtVerifier::jwks(client(&server).min_refresh_interval(Duration::from_secs(3600)))
    .issuer(ISSUER);
  assert!(run(limited.verify(&t)).await.is_err());
  assert!(matches!(
    run(limited.verify(&t)).await,
    Err(AuthError::Unavailable(_))
  ));

  publish(&server, &[&issuer]).await;
  assert!(run(verifier.verify(&t)).await.is_ok());
}

#[tokio::test]
async fn cached_keys_keep_serving_when_a_refresh_fails() {
  let server = MockServer::start().await;
  let issuer = rsa_issuer("k1");
  publish(&server, &[&issuer]).await;
  // Every lookup finds the cache stale and tries to refresh.
  let jwks = client(&server).max_age(Duration::ZERO);
  let verifier = JwtVerifier::jwks(jwks.clone()).issuer(ISSUER);
  let t = token(&issuer).await;
  run(verifier.verify(&t)).await.unwrap();
  outage(&server).await;
  assert!(run(verifier.verify(&t)).await.is_ok());
  assert_eq!(jwks.fetch_count(), 2);
  assert!(matches!(
    run(jwks.refresh()).await,
    Err(AuthError::Unavailable(_))
  ));
}

#[tokio::test]
async fn m15_jwks_body_capped() {
  let server = MockServer::start().await;
  let issuer = rsa_issuer("k1");
  // A valid document padded past 1 MiB: refused, whatever it holds.
  let pad = "x".repeat(id_effect_auth::jwks::MAX_BODY_BYTES);
  let doc = json!({ "keys": [issuer.jwk().expect("jwk")], "pad": pad });
  Mock::given(method("GET"))
    .and(path("/certs"))
    .respond_with(ResponseTemplate::new(200).set_body_json(doc))
    .mount(&server)
    .await;
  let jwks = client(&server);
  let err = run(jwks.refresh()).await.unwrap_err();
  assert!(
    matches!(&err, AuthError::Unavailable(m) if m.contains("larger than 1048576 bytes")),
    "{err:?}"
  );
  assert!(jwks.cached_keys().is_empty());
  // The same keys in a document of ordinary size are read.
  publish(&server, &[&issuer]).await;
  assert_eq!(run(jwks.refresh()).await, Ok(1));
}

#[tokio::test]
async fn m15_jwks_redirects_and_plain_http_are_refused() {
  let server = MockServer::start().await;
  let issuer = rsa_issuer("k1");
  publish(&server, &[&issuer]).await;
  Mock::given(method("GET"))
    .and(path("/moved"))
    .respond_with(
      ResponseTemplate::new(302).insert_header("location", format!("{}/certs", server.uri())),
    )
    .mount(&server)
    .await;
  let moved = JwksClient::new(format!("{}/moved", server.uri()));
  let err = run(moved.refresh()).await.unwrap_err();
  assert!(
    matches!(&err, AuthError::Unavailable(m) if m.contains("302")),
    "a redirect is not followed: {err:?}"
  );
  // A non-loopback http URL is a configuration error, before any request.
  let plain = JwksClient::new("http://id.example.com/certs");
  let verifier = JwtVerifier::jwks(plain.clone()).issuer(ISSUER);
  let err = run(verifier.verify(&token(&issuer).await))
    .await
    .unwrap_err();
  assert!(
    matches!(&err, AuthError::Config(m) if m.contains("must be https")),
    "{err:?}"
  );
  assert_eq!(err.http_status(), 500);
  assert_eq!(plain.fetch_count(), 1);
}

#[tokio::test]
async fn malformed_documents_and_transport_errors_are_unavailable() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/certs"))
    .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
    .mount(&server)
    .await;
  let err = run(client(&server).refresh()).await.unwrap_err();
  assert!(
    matches!(&err, AuthError::Unavailable(m) if m.contains("not JSON")),
    "{err:?}"
  );

  let nowhere = JwksClient::new("http://127.0.0.1:9/certs").timeout(Duration::from_secs(2));
  assert!(matches!(
    run(nowhere.refresh()).await,
    Err(AuthError::Unavailable(m)) if m.contains("request failed")
  ));
  assert!(format!("{:?}", JwtVerifier::jwks(nowhere)).contains("JWKS http://127.0.0.1:9"));
}
