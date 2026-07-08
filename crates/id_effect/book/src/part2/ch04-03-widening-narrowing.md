# Widening and Narrowing — Environment Transformations

Sometimes your effect needs *part* of an environment, but you have the whole thing. Or you need to thread an effect through a context that provides more than required. This is where `zoom_env`, `contramap_env`, and capability subtyping come in.

## The Mismatch Problem

Imagine your application needs several capabilities:

```rust
// Effect needs only EffectLogger
fn log_event(msg: &str) -> Effect<(), LogError, caps!(EffectLogger)> { ... }

// Caller has Database + EffectLogger + Config
fn process(data: Data) -> Effect<(), AppError, caps!(Database, EffectLogger, Config)> { ... }
```

You can't call `log_event` inside `process` without adapting the environment — the `R` types don't match. You need to *narrow* or *widen*.

## zoom_env: Narrow the Environment

`zoom_env` adapts an effect to work with a *larger* environment by providing a lens from the larger type to the smaller one:

```rust
// Adapt log_event to work with a struct holding logger + other fields
let app_log = log_event("hello").zoom_env(|env: &AppEnv| &env.logger);
```

Now `app_log` has type `Effect<(), LogError, AppEnv>`. The function extracts the `Logger` from `AppEnv` and feeds it to the original effect.

Inside `effect!`, the pattern looks like:

```rust
fn process(data: Data) -> Effect<(), AppError, caps!(Database, EffectLogger, Config)> {
    effect!(|r| {
        ~ log_event("start").zoom_env(|e| extract_logger(e)).map_error(AppError::Log);
        ~ db_query(data).zoom_env(|e| extract_db(e)).map_error(AppError::Db);
        Ok(())
    })
}
```

## contramap_env: Transform the Environment

While `zoom_env` narrows, `contramap_env` transforms. It applies a function to convert whatever environment the caller provides into what the effect actually needs:

```rust
// Effect needs a raw string URL
fn connect(url: &str) -> Effect<Database, DbError, String> { ... }

// You have a Config that contains the URL
let with_config = connect_raw.contramap_env(|cfg: &Config| cfg.db_url.clone());
// Now type is Effect<Database, DbError, Config>
```

`contramap_env` is the formal name for "adapt the environment type." In practice, most code uses `zoom_env` for the common case of extracting a field.

## caps! and automatic subtyping

For capability DI, prefer [`caps!`](../../src/capability/set.rs) and the `~` bind operator inside `effect!`. A wider runtime environment satisfies a narrower `R`: every [`CapList`](../../src/capability/set.rs) shares one [`Env`](../../src/capability/env.rs); [`cap_into_bind`](../../src/capability/cap_bind.rs) clones that env and verifies the inner keys (see ADR 0005).

```rust
use id_effect::{Effect, caps, effect, run_with, provide};

fn query(id: u64) -> Effect<User, DbError, caps!(Database)> {
    effect!(|r| {
        let db = ~Database;
        db.fetch_user(id)
    })
}

fn log_event(msg: &str) -> Effect<(), LogError, caps!(EffectLogger)> {
    effect!(|r| {
        let log = ~EffectLogger;
        log.info(msg);
    })
}

fn app() -> Effect<User, AppError, caps!(Database, EffectLogger)> {
    effect!(|r| {
        ~log_event("start");
        ~query(42)
    })
}

run_with(
    [provide!(DatabaseLive), provide!(LoggerLive)],
    app(),
)?;
```

**Automatic binding with `~`:** when the inner effect needs any single key from the outer `caps!(…)` list, `effect!` expands `~inner(...)` to [`cap_into_bind`](../../src/capability/cap_bind.rs) — no manual projection.

**Capability lookup:** `~Database` inside `effect!` borrows that capability from `r` (same as [`require!(Database)`](../../src/capability/require.rs)).

**Implicit `|r|`:** write `effect!(|r| { … })`; Rust infers `&mut caps!(…)` from the enclosing function's `Effect<_, _, caps!(…)>` return type. You still declare `caps!(…)` once on the signature.

**Outside `effect!`**, narrow explicitly with [`CapWiden::widen`](../../src/capability/set.rs) or [`project_at_*`](../../src/capability/set.rs):

```rust
let wide: caps!(Database, EffectLogger) = /* from build_env or run_with */;
let narrow: caps!(Database) = wide.widen();
run_blocking(query(1), narrow)?;
```

When an inner function declares `caps!(Database)` and the caller holds `caps!(Database, EffectLogger)`, the compiler checks that every required key is present — no positional tuple indexing, no runtime downcasts.

## R as Documentation Revisited

These combinators highlight why `R` is valuable as documentation. When you see:

```rust
fn log_event(msg: &str) -> Effect<(), LogError, caps!(EffectLogger)>
```

You know *exactly* what this function needs. You don't need to read its body to see if it also touches the database. Adaptation at the call site stays explicit.

Compare to the pre-effect alternative:

```rust
// Traditional: you'd need to read the body to know what `env` is used for
fn log_event(env: &AppEnv, msg: &str) -> Result<(), LogError> { ... }
```

With `R`, the function declares what it needs. With `zoom_env` or `CapWiden`, the caller declares how to satisfy it.

## When to Use These

In practice, `zoom_env` and `contramap_env` appear most often in *library code* — when writing reusable utilities that should work with any environment containing the right piece. Application code typically uses [`caps!`](../../src/capability/set.rs) / [`CapWiden`](../../src/capability/set.rs) and named capability services (Chapters 5–6), which avoid the need for explicit projection.

Think of `zoom_env` as the manual fallback when automatic capability subtyping isn't the right fit.
