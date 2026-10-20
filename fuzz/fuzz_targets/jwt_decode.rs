//! JWT header and claims decode (id_effect_auth `JwtVerifier::verify`, feature `jwt`).
//!
//! The input is used twice: as a raw bearer token (header parsing, key selection and the
//! malformed-token paths), and split at `\n---\n` into a header and a claims document that are
//! base64url-encoded and HS256-signed with the verifier's secret, so the signature holds and the
//! claims decode, the `exp`/`nbf`/`iss`/`aud` checks and the claim mapping all see fuzzed JSON.
//!
//! Checks besides "no panic": a raw token the fuzzer could not have signed is never accepted.
#![no_main]

use std::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use id_effect::{Exit, UtcDateTime, WallClock};
use id_effect_auth::{ClaimMapping, JwtKey, JwtVerifier};
use id_effect_fuzz::split_two;
use libfuzzer_sys::fuzz_target;
use sha2::Sha256;

const SECRET: &[u8] = b"a 256-bit secret for HS256 fuzzing";

fn verifier() -> JwtVerifier {
  // A fixed instant, so `exp` and `nbf` decide the same way on every run.
  let clock = WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_800_000_000_000));
  JwtVerifier::new([
    JwtKey::hmac(SECRET).with_kid("k1"),
    JwtKey::hmac(b"other key"),
  ])
  .issuer("https://id.example.com")
  .audience("ledger")
  .require_claim("realm_access.roles")
  .require_exp(false)
  .claims(ClaimMapping::default().roles(["roles", "realm_access.roles"]))
  .with_clock(clock)
}

fn signed(header: &[u8], claims: &[u8]) -> String {
  let signing_input = format!(
    "{}.{}",
    URL_SAFE_NO_PAD.encode(header),
    URL_SAFE_NO_PAD.encode(claims)
  );
  let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).expect("HMAC takes any key length");
  mac.update(signing_input.as_bytes());
  let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
  format!("{signing_input}.{signature}")
}

fuzz_target!(|data: &[u8]| {
  let verifier = verifier();
  if let Ok(raw) = std::str::from_utf8(data)
    && let Exit::Success(principal) = verifier.verify(raw).block_on()
  {
    // Only a token carrying a valid HS256 signature can pass; the fuzzer cannot forge one
    // except by replaying a seed that was signed offline.
    panic!("an unsigned fuzzed token was accepted: {principal:?}");
  }
  let (header, claims) = split_two(data);
  let _ = verifier.verify(&signed(header, claims)).block_on();
});
