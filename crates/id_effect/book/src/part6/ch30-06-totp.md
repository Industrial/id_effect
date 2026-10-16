# Two-factor authentication with TOTP (`id_effect_auth`)

A password proves something the user knows. A **time-based one-time password** (TOTP, RFC 6238)
adds something the user has: an authenticator app holding a secret shared with the service, which
shows a fresh 6-digit code every 30 seconds. `id_effect_auth` implements it behind the `totp`
feature, over the totp-rs crate:

| Piece | What it does |
|---|---|
| `Totp::enrol` | a random 160-bit `TotpSecret` and the `otpauth://` URI the app scans |
| `Totp::verify` | checks a code on the effect runtime's clock, ±1 step, each step accepted once |
| `TotpStepStore` | the last step each user consumed (`MemoryTotpStepStore` for tests) |
| `RecoveryCodes` | single-use backup codes, stored as argon2id hashes |
| `Totp::step_up`, `with_mfa_claims`, `require_mfa` | mark the session, expose `amr`, require it in a policy |

## Enrolment

Generate a secret, show it to the user once (as a QR code of the URI, or the base32 text for
typing), and store it only after the user has typed a first valid code:

```rust
use id_effect_auth::{MemoryTotpStepStore, Totp, TotpConfig};

let totp = Totp::new(TotpConfig::new("Ledger"), MemoryTotpStepStore::new());
let enrolment = totp.enrol("alice@example.com").unwrap();

let uri = enrolment.uri(); // render as a QR code; never log it
assert!(uri.starts_with("otpauth://totp/Ledger:alice%40example.com?secret="));
let stored = enrolment.secret().to_base32(); // keep encrypted at rest, with the user record
assert_eq!(stored.len(), 32); // 160 bits
```

`TotpConfig::new` uses the parameters every authenticator app supports: HMAC-SHA-1, 6 digits,
30-second steps and one step of skew. Its fields (`algorithm`, `digits`, `step_secs`, `skew`) can
be changed; non-default values are written into the URI. An issuer or account name containing
`:` cannot be put in the URI's label and is refused with `TotpError::Config`. `TotpSecret` and
`Enrolment` never print their contents with `Debug`.

## Verification

`Totp::verify(user_id, secret, code)` is an effect returning a `TotpVerification`:

- **`Valid`** — the code matches the current step or the one before or after it (the skew
  window RFC 6238 §5.2 recommends for clock drift and typing time), and that step is later than
  the last one this user consumed. The step is now consumed.
- **`Replayed`** — the code matches, but for a step the user already consumed or an earlier one.
  RFC 6238 requires that a code be accepted only once; an attacker who watched the user type it
  cannot reuse it.
- **`Invalid`** — no match in the window.

Spaces in the code are ignored. The step store's `consume(user_id, step)` is a compare-and-set
(record the step only if it is later than the recorded one); implement `TotpStepStore` over the
user table for production, so several instances share it.

The time comes from a `WallClock` (`Totp::with_clock`), so tests run in virtual time. On a
`TestExecutor`, anchor the clock at the executor's instant and move it with `advance`:

```rust
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::{Effect, Executor, Exit, TestExecutor, UtcDateTime, WallClock};
use id_effect_auth::{MemoryTotpStepStore, Totp, TotpConfig, TotpSecret, TotpVerification};

/// Runs `eff` on `exec` until it stalls and returns its exit.
fn run<A: Send + 'static, E: Send + 'static>(exec: &TestExecutor, eff: Effect<A, E>) -> Exit<A, E> {
  let fiber = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  let mut join = std::pin::pin!(fiber.join_future());
  match join.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(exit) => exit,
    Poll::Pending => panic!("still running"),
  }
}

let exec = TestExecutor::new();
let t0 = UtcDateTime::unsafe_make(1_800_000_015_000); // 15 s into a step
let totp = Totp::new(TotpConfig::new("Ledger"), MemoryTotpStepStore::new())
  .with_clock(WallClock::anchored(exec.now(), t0));
let secret = TotpSecret::generate().unwrap();
let code = totp.code_at(&secret, t0).unwrap(); // what the app shows at t0

// One step later the code is still within the window ...
exec.advance(Duration::from_secs(30));
let check = run(&exec, totp.verify("alice", &secret, &code));
assert!(matches!(check, Exit::Success(TotpVerification::Valid)));
// ... but only once.
let again = run(&exec, totp.verify("alice", &secret, &code));
assert!(matches!(again, Exit::Success(TotpVerification::Replayed)));
// Two steps later it has expired.
exec.advance(Duration::from_secs(30));
let late = run(&exec, totp.verify("bob", &secret, &code));
assert!(matches!(late, Exit::Success(TotpVerification::Invalid)));
```

Verification does not count failures. Throttle attempts per user (with `id_effect_axum`'s rate
limiting, or a counter next to the secret): a 6-digit code over a three-step window is guessed
with probability 3 · 10⁻⁶ per attempt.

## Recovery codes

A user who loses the device needs another way in. `RecoveryCodes::generate(n)` returns `n`
codes such as `K7Q2M-XW9TR` (50 random bits from an alphabet without `I`, `O`, `0`, `1`) and
their argon2id hashes, computed by the `PasswordHasher` on the blocking pool. Show the codes once
and store only the hashes:

```rust
use id_effect::Exit;
use id_effect_auth::totp::Redemption;
use id_effect_auth::{PasswordHasher, PasswordParams, RecoveryCodes};

// Low cost for the example; production uses `RecoveryCodes::default()` (OWASP argon2id).
let codes = RecoveryCodes::new(PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap()));
let Exit::Success(generated) = codes.generate(2).block_on() else { panic!() };
let shown = generated.codes[0].expose().to_owned();

// The user types it (case and the dash do not matter).
let typed = shown.to_lowercase().replace('-', "");
let Exit::Success(Redemption::Redeemed { remaining }) =
  codes.redeem(&typed, generated.hashes.clone()).block_on()
else {
  panic!()
};
assert_eq!(remaining.len(), 1); // store `remaining`: the code is spent
let Exit::Success(again) = codes.redeem(&shown, remaining).block_on() else { panic!() };
assert_eq!(again, Redemption::Invalid);
```

`redeem` verifies the code against **every** stored hash, whichever matches, so the time it
takes does not reveal the position of a match; each comparison is argon2's constant-time one.
Generating a new set replaces the old one.

## Step-up and policies

After a valid code (or recovery code), call `Totp::step_up(&sessions, session)`. It records the
time in the session attribute `mfa_verified_at`, saves the session and rotates its id (a
privilege change, like a login), and returns the new session (or `None` if it ended meanwhile);
send its `Sessions::set_cookie`.

Policies read principals, not sessions. `with_mfa_claims(principal, &session)` adds the RFC 8176
authentication methods `otp` and `mfa` to the principal's `amr` claim (merging with any already
there, as a JWT from a provider that did MFA carries) and the time as `mfa_at`. `require_mfa()`
is the `id_effect_authz` claim policy `claim_contains("amr", "mfa")`:

```rust
use id_effect::Exit;
use id_effect_auth::AuthPrincipal;
use id_effect_auth::totp::require_mfa;
use id_effect_authz::{CurrentPrincipal, Decision, Policy, PolicyExt, Rbac};

let rbac = Rbac::builder().grant("accountant", ["ledger:close"]).build();
let close_period = rbac.require("ledger:close").and(require_mfa());

let alice = AuthPrincipal::new("alice").with_role("accountant");
let password_only = CurrentPrincipal::new(alice.clone());
assert!(matches!(
  close_period.evaluate(&password_only, &()).block_on(),
  Exit::Success(Decision::Deny(_))
));
// What `with_mfa_claims` adds after a step-up:
let stepped_up = CurrentPrincipal::new(alice.with_claim("amr", vec!["otp", "mfa"]));
assert_eq!(
  close_period.evaluate(&stepped_up, &()).block_on(),
  Exit::Success(Decision::Allow)
);
```

For a sensitive action long after the login, `is_mfa_fresh(&session, now, max_age)` says whether
the second factor is recent enough or the user should be asked for a code again.

## Tests

`crates/id_effect_auth/tests/totp.rs` checks the RFC 6238 Appendix B vectors (SHA-1, SHA-256 and
SHA-512 at the six reference times), the ±1 step window and its movement under
`TestExecutor::advance`, replay of the same and an earlier step, the `otpauth://` URI, recovery
codes being argon2id hashes that redeem once (case- and dash-insensitive), and a step-up whose
claims turn an `require_mfa` denial into an allow.

```text
cargo nextest run -p id_effect_auth --features totp
```
