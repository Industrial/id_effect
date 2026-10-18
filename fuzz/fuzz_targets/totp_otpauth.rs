//! TOTP inputs (id_effect_auth `totp`, feature `totp`): the parts of an `otpauth://` enrolment
//! and a login that come from outside: the base32 secret (as stored or as in an `otpauth` URI),
//! the account name written into the URI, and the code a user types. The input is split at the
//! first two newlines into secret, account and code; the last byte of the secret line picks the
//! code length (5 to 9 digits, so the rejected lengths are covered too). A secret line that is
//! not base32 is used as raw secret bytes.
//!
//! id_effect_auth builds `otpauth` URIs but parses none, so the secret, URI label and code
//! parsers are what this target covers.
//!
//! Checks besides "no panic": a base32 secret survives `to_base32`; a URI starts with
//! `otpauth://totp/` and carries the secret; the code for now verifies once and then is a
//! replay.
#![no_main]

use std::time::Instant;

use id_effect::{Exit, UtcDateTime, WallClock};
use id_effect_auth::{MemoryTotpStepStore, Totp, TotpConfig, TotpSecret, TotpVerification};
use libfuzzer_sys::fuzz_target;

const NOW_MS: i64 = 1_800_000_000_000;

fuzz_target!(|data: &[u8]| {
  let mut parts = data.splitn(3, |&b| b == b'\n');
  let secret_line = parts.next().unwrap_or_default();
  let account = String::from_utf8_lossy(parts.next().unwrap_or_default());
  let typed = String::from_utf8_lossy(parts.next().unwrap_or_default());

  let secret = match std::str::from_utf8(secret_line)
    .ok()
    .and_then(|s| TotpSecret::from_base32(s).ok())
  {
    Some(secret) => {
      let again = TotpSecret::from_base32(&secret.to_base32()).expect("to_base32 re-parses");
      assert_eq!(again, secret, "base32 round trip changed the secret");
      secret
    }
    None => TotpSecret::from_bytes(secret_line),
  };

  let mut config = TotpConfig::new("Acme");
  config.digits = 5 + secret_line.last().copied().unwrap_or(1) % 5;
  let clock = WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(NOW_MS));
  let totp = Totp::new(config, MemoryTotpStepStore::new()).with_clock(clock);

  if let Ok(uri) = totp.uri(&secret, &account) {
    assert!(uri.starts_with("otpauth://totp/"), "{uri}");
    assert!(uri.contains(&secret.to_base32()), "{uri}");
  }
  let _ = totp.verify("mallory", &secret, &typed).block_on();
  if let Ok(code) = totp.code_at(&secret, UtcDateTime::unsafe_make(NOW_MS)) {
    let first = totp.verify("alice", &secret, &code).block_on();
    assert!(
      matches!(first, Exit::Success(TotpVerification::Valid)),
      "the current code did not verify: {first:?}"
    );
    let second = totp.verify("alice", &secret, &code).block_on();
    assert!(
      matches!(second, Exit::Success(TotpVerification::Replayed)),
      "a used code was not a replay: {second:?}"
    );
  }
});
