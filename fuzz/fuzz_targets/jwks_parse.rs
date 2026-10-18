//! JWKS document parse (id_effect_auth, feature `jwt`): the input is a JSON Web Key Set read the
//! way `JwksClient` reads a fetched one (the `keys` array, `"use": "enc"` keys skipped, each key
//! through the public `Jwk` type and `JwtKey::from_jwk`), and as a whole `JwkSet`. The usable keys
//! then verify tokens of each algorithm family, so fuzzed RSA moduli, EC points and Ed25519 keys
//! reach the signature check.
//!
//! `JwksClient` itself fetches over HTTP; its parser is private, so this target drives the same
//! public steps it is built from.
#![no_main]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use id_effect_auth::JwtVerifier;
use id_effect_auth::jwt::{Jwk, JwkSet, JwtKey};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

/// A token with header `alg` and a zero signature of `sig_len` bytes.
fn token(alg: &str, sig_len: usize) -> String {
  let header = URL_SAFE_NO_PAD.encode(format!(r#"{{"alg":"{alg}","kid":"k1"}}"#));
  let claims = URL_SAFE_NO_PAD.encode(r#"{"sub":"alice","exp":4102444800}"#);
  let signature = URL_SAFE_NO_PAD.encode(vec![0_u8; sig_len]);
  format!("{header}.{claims}.{signature}")
}

fuzz_target!(|data: &[u8]| {
  let _ = serde_json::from_slice::<JwkSet>(data);
  let Ok(doc) = serde_json::from_slice::<Value>(data) else {
    return;
  };
  let Some(keys) = doc.get("keys").and_then(Value::as_array) else {
    return;
  };
  let usable: Vec<JwtKey> = keys
    .iter()
    .filter(|k| k.get("use").and_then(Value::as_str) != Some("enc"))
    .filter_map(|k| serde_json::from_value::<Jwk>(k.clone()).ok())
    .filter_map(|jwk| JwtKey::from_jwk(&jwk).ok())
    .collect();
  if usable.is_empty() {
    return;
  }
  let verifier = JwtVerifier::new(usable);
  for (alg, sig_len) in [
    ("HS256", 32),
    ("RS256", 256),
    ("PS256", 256),
    ("ES256", 64),
    ("ES384", 96),
    ("EdDSA", 64),
  ] {
    let _ = verifier.verify(&token(alg, sig_len)).block_on();
  }
});
