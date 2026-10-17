//! TOTP through the public API (feature `totp`): the RFC 6238 Appendix B test vectors, ±1 step
//! of skew and replay protection in virtual time on a `TestExecutor`, the `otpauth://` URI,
//! argon2-hashed single-use recovery codes, and the MFA step-up read by an authz policy.

#![cfg(feature = "totp")]

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::{Cause, Effect, Executor, Exit, TestExecutor, UtcDateTime, WallClock};
use id_effect_auth::totp::{
  AMR_CLAIM, Algorithm, MFA_VERIFIED_AT_ATTR, Redemption, is_mfa_fresh, mfa_verified_at,
  require_mfa, with_mfa_claims,
};
use id_effect_auth::{
  AuthPrincipal, MemorySessionStore, MemoryTotpStepStore, PasswordHasher, PasswordParams,
  RecoveryCodes, SessionConfig, Sessions, Totp, TotpConfig, TotpSecret, TotpVerification,
};
use id_effect_authz::{CurrentPrincipal, Decision, Policy, Principal};

fn ok<A, E: std::fmt::Debug>(exit: Exit<A, E>) -> A {
  match exit {
    Exit::Success(a) => a,
    Exit::Failure(Cause::Fail(e)) => panic!("failed: {e:?}"),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// Runs `eff` as a root fiber on `exec` until it stalls; it must have finished.
fn on<A: Send + 'static, E: Send + 'static>(exec: &TestExecutor, eff: Effect<A, E>) -> Exit<A, E> {
  let handle = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(handle.is_done(), "the effect is still waiting");
  let mut join = pin!(handle.join_future());
  match join.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(exit) => exit,
    Poll::Pending => unreachable!("done fibers are joinable"),
  }
}

fn secs(s: i64) -> UtcDateTime {
  UtcDateTime::unsafe_make(s * 1000)
}

/// Low-cost argon2id so the suite stays fast; production uses `PasswordHasher::default()`.
fn fast_hasher() -> PasswordHasher {
  PasswordHasher::new(PasswordParams::new(8, 1, 1).expect("valid params"))
}

/// RFC 6238 Appendix B: (time, SHA-1, SHA-256, SHA-512) codes, 8 digits, 30 s steps.
const RFC6238: [(i64, &str, &str, &str); 6] = [
  (59, "94287082", "46119246", "90693936"),
  (1_111_111_109, "07081804", "68084774", "25091201"),
  (1_111_111_111, "14050471", "67062674", "99943326"),
  (1_234_567_890, "89005924", "91819424", "93441116"),
  (2_000_000_000, "69279037", "90698825", "38618901"),
  (20_000_000_000, "65353130", "77737706", "47863826"),
];

/// The RFC's seeds: the ASCII digits repeated to the hash's block of 20, 32 or 64 bytes.
fn rfc_totp(algorithm: Algorithm, clock: WallClock) -> (Totp, TotpSecret) {
  let len = match algorithm {
    Algorithm::SHA1 => 20,
    Algorithm::SHA256 => 32,
    _ => 64,
  };
  let seed: Vec<u8> = b"1234567890".iter().copied().cycle().take(len).collect();
  let config = TotpConfig {
    algorithm,
    digits: 8,
    ..TotpConfig::new("RFC")
  };
  let totp = Totp::new(config, MemoryTotpStepStore::new()).with_clock(clock);
  (totp, TotpSecret::from_bytes(&seed))
}

#[test]
fn rfc6238_test_vectors() {
  for (time, sha1, sha256, sha512) in RFC6238 {
    for (algorithm, expected) in [
      (Algorithm::SHA1, sha1),
      (Algorithm::SHA256, sha256),
      (Algorithm::SHA512, sha512),
    ] {
      let clock = WallClock::anchored(Instant::now(), secs(time));
      let (totp, secret) = rfc_totp(algorithm, clock);
      assert_eq!(
        totp.code_at(&secret, secs(time)).unwrap(),
        expected,
        "{algorithm} at {time}"
      );
      // The same code verifies on a clock reading that time.
      let check = ok(totp.verify("rfc", &secret, expected).block_on());
      assert_eq!(check, TotpVerification::Valid, "{algorithm} at {time}");
    }
  }
}

/// A `Totp` on `exec`'s virtual clock, which reads `start` now.
fn on_test_clock(exec: &TestExecutor, start: UtcDateTime) -> (Totp, MemoryTotpStepStore) {
  let steps = MemoryTotpStepStore::new();
  let clock = WallClock::anchored(exec.now(), start);
  let totp = Totp::new(TotpConfig::new("Acme"), steps.clone()).with_clock(clock);
  (totp, steps)
}

/// 15 s into the step starting at 1_800_000_000 (step 60_000_000).
const T0: i64 = 1_800_000_015;

#[test]
fn skew_accepts_one_step_either_side_in_virtual_time() {
  let exec = TestExecutor::new();
  let (totp, _) = on_test_clock(&exec, secs(T0));
  let secret = TotpSecret::generate().unwrap();
  let code = |dt: i64| totp.code_at(&secret, secs(T0 + dt)).unwrap();
  let verify = |user: &str, code: String| ok(on(&exec, totp.verify(user, &secret, &code)));

  assert_eq!(verify("now", code(0)), TotpVerification::Valid);
  assert_eq!(verify("prev", code(-30)), TotpVerification::Valid);
  assert_eq!(verify("next", code(30)), TotpVerification::Valid);
  assert_eq!(verify("two-back", code(-60)), TotpVerification::Invalid);
  assert_eq!(verify("two-ahead", code(60)), TotpVerification::Invalid);

  // The virtual clock moves the window: a step later, the T0 code is the previous step's ...
  let issued = code(0);
  exec.advance(Duration::from_secs(30));
  assert_eq!(verify("late", issued.clone()), TotpVerification::Valid);
  // ... and two steps later it has expired.
  exec.advance(Duration::from_secs(30));
  assert_eq!(verify("later", issued), TotpVerification::Invalid);
}

#[test]
fn a_code_is_accepted_once_and_earlier_steps_never_after() {
  let exec = TestExecutor::new();
  let (totp, steps) = on_test_clock(&exec, secs(T0));
  let secret = TotpSecret::generate().unwrap();
  let code = |dt: i64| totp.code_at(&secret, secs(T0 + dt)).unwrap();
  let verify = |code: String| ok(on(&exec, totp.verify("alice", &secret, &code)));

  assert_eq!(verify(code(0)), TotpVerification::Valid);
  assert_eq!(steps.last_step("alice"), Some(T0 as u64 / 30));
  assert_eq!(verify(code(0)), TotpVerification::Replayed, "same step");
  assert_eq!(
    verify(code(-30)),
    TotpVerification::Replayed,
    "previous step"
  );
  assert_eq!(verify(code(30)), TotpVerification::Valid, "next step");
  assert_eq!(verify(code(0)), TotpVerification::Replayed);
  // Users are independent.
  let other = ok(on(&exec, totp.verify("bob", &secret, &code(0))));
  assert_eq!(other, TotpVerification::Valid);
  // Once the clock reaches a new step, its code is fresh.
  exec.advance(Duration::from_secs(60));
  assert_eq!(verify(code(60)), TotpVerification::Valid);
  assert!(TotpVerification::Valid.is_valid() && !TotpVerification::Replayed.is_valid());
}

#[test]
fn enrolment_yields_a_secret_and_an_otpauth_uri() {
  let totp = Totp::new(TotpConfig::new("Acme Corp"), MemoryTotpStepStore::new());
  let enrolment = totp.enrol("alice@example.com").unwrap();
  let secret = enrolment.secret().to_base32();
  assert_eq!(enrolment.secret().as_bytes().len(), 20, "160-bit secret");
  let uri = enrolment.uri();
  assert!(
    uri.starts_with("otpauth://totp/Acme%20Corp:alice%40example.com?"),
    "{uri}"
  );
  assert!(uri.contains(&format!("secret={secret}")), "{uri}");
  assert!(uri.contains("issuer=Acme%20Corp"), "{uri}");
  assert_eq!(format!("{enrolment:?}"), "Enrolment(<redacted>)");
  assert_ne!(
    totp.enrol("alice@example.com").unwrap().secret(),
    enrolment.secret()
  );

  // Non-default parameters are spelled out for the app.
  let config = TotpConfig {
    algorithm: Algorithm::SHA256,
    digits: 8,
    step_secs: 60,
    ..TotpConfig::new("Acme")
  };
  let totp = Totp::new(config, MemoryTotpStepStore::new());
  let uri = totp.uri(enrolment.secret(), "alice").unwrap();
  for param in ["algorithm=SHA256", "digits=8", "period=60"] {
    assert!(uri.contains(param), "{uri}");
  }
  // An account name with a colon cannot be put in the label.
  assert!(totp.enrol("a:b").is_err());
}

#[test]
fn recovery_codes_are_hashed_and_single_use() {
  let exec = TestExecutor::new();
  let codes = RecoveryCodes::new(fast_hasher());
  let generated = ok(on(&exec, codes.generate(3)));
  assert_eq!(generated.codes.len(), 3);
  assert_eq!(generated.hashes.len(), 3);
  for (code, hash) in generated.codes.iter().zip(&generated.hashes) {
    assert!(hash.as_str().starts_with("$argon2id$"), "{hash}");
    assert!(!hash.as_str().contains(code.expose()));
  }
  let first = generated.codes[1].expose().to_owned();

  // Typed in lower case without the dash: still the same code.
  let typed = first.replace('-', "").to_lowercase();
  let Redemption::Redeemed { remaining } =
    ok(on(&exec, codes.redeem(&typed, generated.hashes.clone())))
  else {
    panic!("a generated code redeems");
  };
  assert_eq!(remaining.len(), 2);
  assert!(!remaining.contains(&generated.hashes[1]));

  // Against the stored remainder, the same code no longer works; the others still do.
  let again = ok(on(&exec, codes.redeem(&first, remaining.clone())));
  assert_eq!(again, Redemption::Invalid);
  let other = generated.codes[0].expose().to_owned();
  let Redemption::Redeemed { remaining } = ok(on(&exec, codes.redeem(&other, remaining))) else {
    panic!("an unused code redeems");
  };
  assert_eq!(remaining, vec![generated.hashes[2].clone()]);
  assert_eq!(
    ok(on(&exec, codes.redeem("ZZZZZ-ZZZZZ", remaining))),
    Redemption::Invalid
  );
  assert_eq!(
    ok(on(&exec, codes.redeem(&other, Vec::new()))),
    Redemption::Invalid
  );
}

#[test]
fn step_up_feeds_an_mfa_policy() {
  let exec = TestExecutor::new();
  let clock = WallClock::anchored(exec.now(), secs(T0));
  let totp =
    Totp::new(TotpConfig::new("Acme"), MemoryTotpStepStore::new()).with_clock(clock.clone());
  let sessions =
    Sessions::new(MemorySessionStore::new(), SessionConfig::default()).with_clock(clock);
  let secret = TotpSecret::generate().unwrap();

  let session = ok(on(&exec, sessions.login("alice", None)));
  let before = with_mfa_claims(AuthPrincipal::from_session(&session), &session);
  let policy = require_mfa();
  let deny = ok(
    policy
      .evaluate(&CurrentPrincipal::new(before), &())
      .block_on(),
  );
  assert!(matches!(deny, Decision::Deny(_)), "{deny:?}");

  // Second factor 10 s later, then the step-up.
  exec.advance(Duration::from_secs(10));
  let code = totp.code_at(&secret, secs(T0 + 10)).unwrap();
  assert!(ok(on(&exec, totp.verify("alice", &secret, &code))).is_valid());
  let stepped = ok(on(&exec, totp.step_up(&sessions, session.clone()))).expect("live session");
  assert_ne!(stepped.id(), session.id(), "id rotated");
  assert_eq!(mfa_verified_at(&stepped), Some(secs(T0 + 10)));
  assert_eq!(
    stepped.attr(MFA_VERIFIED_AT_ATTR),
    Some((T0 + 10).to_string().as_str())
  );

  let loaded = ok(on(&exec, sessions.load(stepped.id()))).expect("persisted");
  let principal = with_mfa_claims(AuthPrincipal::from_session(&loaded), &loaded);
  assert_eq!(
    principal.claim(AMR_CLAIM),
    Some(&serde_json::json!(["otp", "mfa"]))
  );
  let allow = ok(
    policy
      .evaluate(&CurrentPrincipal::new(principal), &())
      .block_on(),
  );
  assert_eq!(allow, Decision::Allow);

  // Freshness for re-prompting before sensitive actions.
  let five_min = Duration::from_secs(300);
  assert!(is_mfa_fresh(&loaded, secs(T0 + 310), five_min));
  assert!(!is_mfa_fresh(&loaded, secs(T0 + 311), five_min));

  // A session that ended in the meantime cannot be stepped up.
  ok(on(&exec, sessions.logout(stepped.id())));
  assert_eq!(ok(on(&exec, totp.step_up(&sessions, stepped))), None);
}
