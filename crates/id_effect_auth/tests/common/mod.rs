//! Test keys: RSA and EC issuers generated at runtime (no key material in the repository).
#![allow(dead_code)]

use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der, PublicKeyX509Der};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::KeyPair as _;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use id_effect::{Cause, Effect, Exit};
use id_effect_auth::{Algorithm, AuthError, JwtIssuer};

/// `der` as a PEM block labelled `label`.
pub fn pem(label: &str, der: &[u8]) -> String {
  let body = STANDARD.encode(der);
  let lines: Vec<&str> = body
    .as_bytes()
    .chunks(64)
    .map(|c| std::str::from_utf8(c).expect("base64 is ASCII"))
    .collect();
  format!(
    "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
    lines.join("\n")
  )
}

/// A fresh RS256 issuer named `kid` (2048-bit key, PKCS#8 PEM) and its public key in PEM.
pub fn rsa_keys(kid: &str) -> (JwtIssuer, String) {
  let pair = KeyPair::generate(KeySize::Rsa2048).expect("rsa keygen");
  let der: Pkcs8V1Der<'static> = pair.as_der().expect("pkcs8");
  let public: PublicKeyX509Der<'static> = pair.public_key().as_der().expect("spki");
  let issuer = JwtIssuer::rsa_pem(
    Algorithm::RS256,
    pem("PRIVATE KEY", der.as_ref()).as_bytes(),
  )
  .expect("rsa issuer")
  .kid(kid);
  (issuer, pem("PUBLIC KEY", public.as_ref()))
}

/// A fresh RS256 issuer named `kid`.
pub fn rsa_issuer(kid: &str) -> JwtIssuer {
  rsa_keys(kid).0
}

/// A fresh ES256 issuer named `kid` (P-256 key, PKCS#8 PEM) and its public key in PEM.
pub fn ec_keys(kid: &str) -> (JwtIssuer, String) {
  let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
    .expect("ec keygen");
  let pair =
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, doc.as_ref()).expect("ec pair");
  let public: PublicKeyX509Der<'static> = pair.public_key().as_der().expect("spki");
  let issuer = JwtIssuer::ec_pem(
    Algorithm::ES256,
    pem("PRIVATE KEY", doc.as_ref()).as_bytes(),
  )
  .expect("ec issuer")
  .kid(kid);
  (issuer, pem("PUBLIC KEY", public.as_ref()))
}

/// A fresh ES256 issuer named `kid`.
pub fn ec_issuer(kid: &str) -> JwtIssuer {
  ec_keys(kid).0
}

/// Runs `eff` synchronously, as a `Result`.
pub fn run<A: Send + 'static>(eff: Effect<A, AuthError>) -> Result<A, AuthError> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}
