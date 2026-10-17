//! RS256 and ES256 issue/verify with PEM keys and JWKs, and algorithm-confusion resistance
//! (sec.authn #4).
#![cfg(feature = "jwt")]

mod common;

use common::{ec_keys, rsa_keys, run};
use id_effect_auth::jwt::{EncodingKey, Jwk};
use id_effect_auth::{Algorithm, AuthError, AuthPrincipal, JwtIssuer, JwtKey, JwtVerifier};
use id_effect_authz::Principal;
use serde_json::json;

fn alice() -> AuthPrincipal {
  AuthPrincipal::new("alice").with_role("accountant")
}

#[test]
fn rs256_tokens_verify_with_the_public_pem_and_the_published_jwk() {
  let (issuer, public_pem) = rsa_keys("rsa-1");
  let issuer = issuer.issuer("https://id.example.com").audience("ledger");
  assert_eq!(issuer.algorithm(), Algorithm::RS256);
  let token = run(issuer.issue(&alice())).unwrap();

  let by_pem = JwtVerifier::new([JwtKey::rsa_pem(public_pem.as_bytes()).unwrap()])
    .issuer("https://id.example.com")
    .audience("ledger");
  let p = run(by_pem.verify(&token)).unwrap();
  assert_eq!(
    (p.id(), p.roles()),
    ("alice", &["accountant".to_owned()][..])
  );

  let jwk = issuer.jwk().unwrap();
  let published = serde_json::to_value(&jwk).unwrap();
  assert_eq!(published["kid"], json!("rsa-1"));
  assert_eq!(published["alg"], json!("RS256"));
  assert_eq!(published["use"], json!("sig"));
  assert_eq!(published["kty"], json!("RSA"));
  assert!(published.get("d").is_none(), "no private exponent");
  let key = JwtKey::from_jwk(&jwk).unwrap();
  assert_eq!(
    (key.kid(), key.algorithms()),
    (Some("rsa-1"), &[Algorithm::RS256][..])
  );
  assert!(run(JwtVerifier::new([key]).verify(&token)).is_ok());
}

#[test]
fn es256_tokens_verify_and_other_keys_are_rejected() {
  let (issuer, public_pem) = ec_keys("ec-1");
  let token = run(issuer.issue(&alice())).unwrap();
  let verifier = JwtVerifier::new([JwtKey::ec_pem(public_pem.as_bytes()).unwrap()]);
  assert_eq!(run(verifier.verify(&token)).unwrap().id(), "alice");
  assert!(run(JwtVerifier::new([issuer.verification_key().unwrap()]).verify(&token)).is_ok());

  let (other, _) = ec_keys("ec-1");
  let foreign = run(other.issue(&alice())).unwrap();
  assert_eq!(
    run(verifier.verify(&foreign)),
    Err(AuthError::InvalidCredentials)
  );
}

#[test]
fn a_public_key_is_never_used_as_an_hmac_secret() {
  let (issuer, public_pem) = rsa_keys("rsa-1");
  // The classic confusion attack: sign HS256 with the RSA public key's bytes.
  let forged = run(
    JwtIssuer::hs256(public_pem.as_bytes())
      .kid("rsa-1")
      .issue(&alice()),
  )
  .unwrap();
  let verifier = JwtVerifier::new([issuer.verification_key().unwrap()]);
  assert!(matches!(
    run(verifier.verify(&forged)),
    Err(AuthError::UnknownKey(_))
  ));
  // Nor can an issuer be built for an algorithm outside its key's family.
  assert!(matches!(
    JwtIssuer::new(Algorithm::ES256, EncodingKey::from_secret(b"s")),
    Err(AuthError::Key(_))
  ));
}

#[test]
fn jwks_documents_are_parsed_with_their_kids() {
  let (a, _) = rsa_keys("a");
  let (b, _) = ec_keys("b");
  let doc = json!({"keys": [a.jwk().unwrap(), b.jwk().unwrap()]});
  let set: id_effect_auth::jwt::JwkSet = serde_json::from_value(doc).unwrap();
  let keys: Vec<JwtKey> = set
    .keys
    .iter()
    .map(|k: &Jwk| JwtKey::from_jwk(k).unwrap())
    .collect();
  let verifier = JwtVerifier::new(keys);
  assert!(run(verifier.verify(&run(a.issue(&alice())).unwrap())).is_ok());
  assert!(run(verifier.verify(&run(b.issue(&alice())).unwrap())).is_ok());
  let bad_alg: Jwk = serde_json::from_value(json!({
    "kty": "oct", "k": "c2VjcmV0", "alg": "RSA-OAEP"
  }))
  .unwrap();
  assert!(matches!(JwtKey::from_jwk(&bad_alg), Err(AuthError::Key(_))));
}
