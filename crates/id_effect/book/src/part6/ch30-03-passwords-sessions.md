# Passwords and sessions (`id_effect_auth`)

Password login has two halves: checking a password against what was stored at registration,
and remembering the result across requests. [`id_effect_auth`](https://docs.rs/id_effect_auth)
provides both as capabilities: `PasswordHasher` for the check and `Sessions` for the memory.

## Hashing passwords

`PasswordHasher` stores passwords as argon2id [PHC strings]
(`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`). `PasswordHasher::default()` uses the
parameters from the OWASP Password Storage Cheat Sheet (`PasswordParams::OWASP`: 19 MiB of memory,
2 iterations, 1 lane). Argon2 is slow by design. Each hash and each verification therefore runs as
one job on the executor's blocking pool, and the fiber suspends until the job finishes:

```rust
use id_effect::Exit;
use id_effect_auth::{PasswordHasher, PasswordParams, Verification};

// Tests and docs use a low cost; production code uses `PasswordHasher::default()`.
let hasher = PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap());

let Exit::Success(stored) = hasher.hash("correct horse").block_on() else { panic!() };
assert!(stored.as_str().starts_with("$argon2id$"));

let Exit::Success(v) = hasher.verify("correct horse", &stored).block_on() else { panic!() };
assert_eq!(v, Verification::Valid);
let Exit::Success(v) = hasher.verify("wrong", &stored).block_on() else { panic!() };
assert_eq!(v, Verification::Invalid);
```

A wrong password is a value (`Verification::Invalid`), not an error. `PasswordError` is reserved
for faults such as a malformed stored hash or invalid parameters.

### Upgrading hashes on login

Raise the cost later, or move hashes from another argon2 variant, and old hashes keep verifying.
When the password is correct but the stored hash used other parameters, `verify` returns
`Verification::ValidNeedsRehash(new_hash)`. `new_hash` is the same password under the current
parameters, computed in the same blocking job, so store it:

```rust
use id_effect::Exit;
use id_effect_auth::{PasswordHasher, PasswordParams, Verification};

let old = PasswordHasher::new(PasswordParams::new(8, 1, 1).unwrap());
let new = PasswordHasher::new(PasswordParams::new(32, 1, 1).unwrap());
let Exit::Success(stored) = old.hash("pw").block_on() else { panic!() };

match new.verify("pw", &stored).block_on() {
  Exit::Success(Verification::ValidNeedsRehash(upgraded)) => {
    assert!(upgraded.as_str().contains("m=32"));
    // users.update_password_hash(user_id, upgraded.as_str())
  }
  other => panic!("{other:?}"),
}
```

### Unknown users

A login handler that answers fast for unknown users and slow for known ones reveals which
accounts exist. `verify_or_dummy(password, stored)` takes the stored hash as an `Option`. With
`None` it verifies against a dummy hash built once with the same parameters and returns
`Invalid`, so an unknown user costs the same argon2 work as a wrong password:

```rust
use id_effect::Exit;
use id_effect_auth::{PasswordHasher, PasswordParams, Verification};

let hasher = PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap());
let stored_for_user = None; // the user lookup found nobody
let Exit::Success(v) = hasher.verify_or_dummy("guess", stored_for_user).block_on() else {
  panic!()
};
assert_eq!(v, Verification::Invalid);
```

### As a capability

`PasswordHasher` is a capability like any other. `password_hasher_layer` provides it, and
`hash_password` / `verify_password` read it from `R`:

```rust
use id_effect::{Effect, Exit, HCons, HNil};
use id_effect_auth::password::{
  PasswordError, PasswordHasher, PasswordParams, Verification, hash_password,
  password_hasher_layer, verify_password,
};

type Env = HCons<PasswordHasher, HNil>;

let program: Effect<Verification, PasswordError, Env> =
  hash_password::<Env, _>("pw").flat_map(|stored| verify_password::<Env, _>("pw", Some(stored)));
let hasher = PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap());
let exit = program.provide_layer(password_hasher_layer(hasher)).block_on();
assert!(matches!(exit, Exit::Success(Verification::Valid)));
```

## Server-side sessions

`Sessions` keeps session state on the server, in a `SessionStore`. The browser holds only an
opaque id: 256 bits from the operating system's CSPRNG, encoded as 64 hex digits. `Sessions`
provides the following operations:

| Operation | What it does |
|---|---|
| `login(user_id, presented)` | New session with a **fresh id**. The id the client presented, if any, is removed, so an id planted before login (session fixation) never becomes authenticated. |
| `load(id)` | The live session or `None`. An expired session is removed. A live one has its idle expiry pushed forward (written back at most once per `touch_interval`). |
| `save(session)` | Persists attribute changes (`set_attr` / `remove_attr`). |
| `rotate(id)` | Same user, attributes and expiries under a new id, for use after a privilege change. |
| `logout(id)` / `revoke_all(user_id)` | End one session, or every session of a user (password change, "log out everywhere"). |
| `purge_expired()` | Removes abandoned records. Schedule it with `Effect::repeat`. |

Every session has two expiries. The **idle timeout** ends a session that has not been used for
`idle_timeout` (default 30 minutes). The **absolute timeout** ends it `absolute_timeout` after
login, however much it is used (default 8 hours):

```rust
use std::time::{Duration, Instant};

use id_effect::{Exit, UtcDateTime, WallClock};
use id_effect_auth::{MemorySessionStore, SessionConfig, Sessions};

// An anchored clock lets the example move time; production uses the system clock.
let clock = WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_800_000_000_000));
let config = SessionConfig {
  idle_timeout: Duration::from_secs(600),
  ..SessionConfig::default()
};
let sessions = Sessions::new(MemorySessionStore::new(), config).with_clock(clock.clone());

let Exit::Success(s) = sessions.login("alice", None).block_on() else { panic!() };
clock.jump(Duration::from_secs(300));
assert!(matches!(sessions.load(s.id()).block_on(), Exit::Success(Some(_))));
clock.jump(Duration::from_secs(601)); // unused for longer than the idle timeout
assert!(matches!(sessions.load(s.id()).block_on(), Exit::Success(None)));
```

Times come from a `WallClock`, so tests can move time explicitly as above, or anchor the clock
to a `TestExecutor` and run expiry in virtual time.

### The cookie

`sessions.set_cookie(&session)` renders the `Set-Cookie` value, and `sessions.clear_cookie()`
renders the one that deletes it on logout. `CookieConfig::default()` produces the most restrictive
cookie that still works for login:

```text
__Host-id_session=<id>; Path=/; Max-Age=28800; Secure; HttpOnly; SameSite=Lax
```

- The `__Host-` prefix makes browsers reject the cookie unless it is `Secure`, has `Path=/` and
  has no `Domain`. A sibling subdomain therefore cannot set or overwrite it.
- `HttpOnly` keeps it away from scripts.
- `SameSite=Lax` keeps it off cross-site sub-requests.

For plain-HTTP local development, use a name without the prefix and set `secure: false`.
`sessions.session_id(cookie_header)` finds the id in a `Cookie` request header. It rejects
anything that is not 64 hex digits before the store is consulted.

### Stores

`SessionStore` is an object-safe trait whose methods return effects: `save`, `load`, `remove`,
`remove_user` and `purge_expired`. `SessionStoreService` is its capability and
`provide_session_store(store)` its layer. `sessions_layer(config)` builds `Sessions` over
whichever store the environment provides:

```rust
use id_effect::{Effect, Exit};
use id_effect_auth::session::{
  MemorySessionStore, SessionConfig, SessionError, SessionsEnv, login, provide_session_store,
  sessions_layer,
};

let layer = provide_session_store::<SessionError>(MemorySessionStore::new())
  .provide_to(sessions_layer(SessionConfig::default()));
let program: Effect<String, SessionError, SessionsEnv> =
  login::<SessionsEnv, _>("alice", None).map(|s| s.user_id().to_owned());
assert!(matches!(program.provide_layer(layer).block_on(), Exit::Success(u) if u == "alice"));
```

`MemorySessionStore` keeps records in process. Use it in tests (`fail_with` simulates an outage)
and in single-process development. Production needs a store that survives restarts and is shared
by every instance. A database-backed store implements the same trait.

## Loading the session per request (feature `axum`)

`SessionLayer::new(sessions)` is a Tower layer. It reads the session cookie, `load`s the session,
and when the session is live records it as a request capability (`RequestCaps`, see the
[application host chapter](ch30-00-application.md)) and as a request extension. A request
without a cookie, with an expired session, or arriving during a store outage continues
anonymously. It is never wrongly authenticated.

```rust
use axum::{Router, extract::State, routing::get};
use id_effect::{DynEnv, Effect, succeed};
use id_effect_auth::{Session, SessionLayer};
use id_effect_axum::{RequestCaps, execute_with_request_caps};
# use id_effect_auth::{MemorySessionStore, SessionConfig, Sessions};
# let sessions = Sessions::new(MemorySessionStore::new(), SessionConfig::default());

fn whoami(env: &mut DynEnv) -> Effect<String, std::convert::Infallible, DynEnv> {
  let who = env.try_get::<Session>().map_or_else(|_| "anonymous".into(), |s| s.user_id().into());
  succeed(who)
}

let app: Router = Router::new()
  .route("/me", get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)))
  .layer(SessionLayer::new(sessions.clone()))
  .with_state(DynEnv::new());
```

The crate's `tests/session_layer.rs` runs the complete flow over a socket: a wrong password and
an unknown user get 401, a correct login sets the cookie, a second login rotates the id, and
logout clears the cookie. `examples/005_passwords_sessions.rs` runs the same flow without HTTP.

To turn a session (or a bearer token, an API key or Basic credentials) into the principal that
`id_effect_authz` guards read, use the `Authenticate` layer described in
[Credentials and JWT](ch30-04-credentials-jwt.md).

[PHC strings]: https://github.com/P-H-C/phc-string-format/blob/master/phc-sf-spec.md
