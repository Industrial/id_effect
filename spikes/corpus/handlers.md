# Spike handler corpus

Twenty handler shapes (`H01`–`H20`) plus one edge-wiring shape (`W01`). Each spike implements every
shape in `spikes/kernel_{a,b}/tests/corpus.rs`, over the services in `spike_corpus` (this crate), and
marks it up as described in [README.md](README.md). Shapes are drawn from the 0.6 axum examples
010–060, the sql example `010_axum_sql` and the config examples 003/041, plus the composition and
concurrency shapes the 0.7 kernel must carry (D3–D7).

Every shape lists:

- **From**: the 0.6 source it is distilled from.
- **R**: the capabilities the handler needs. `{}` means none.
- **E**: the error type. `Never` means the kernel's uninhabited error.
- **In**: request input, if any.
- **Do**: the behaviour the corpus test asserts.
- **Region**: what the `corpus:begin`/`corpus:end` region must contain besides the handler itself.
- **Probes**: the design risk the shape is there to expose. These are the reason the shape exists;
  do not route around them (see the fairness rules in the README).

`Ctx` below means "however the design spells the environment", and `~T` means "however the design reads
capability `T`". The shapes are spelled behaviourally on purpose, so that neither design is favoured.

---

### H01 — single capability read

- **From**: axum `010_routing_hello.rs` (`hello(env: &mut AppState)`).
- **R**: `{Greeting}`. **E**: `Never`. **In**: —
- **Do**: return `~Greeting.0.to_string()`; run with `Greeting("hello from effect-axum")`.
- **Region**: the handler and the edge call that provides `Greeting` and runs it.
- **Probes**: the baseline cost of one capability. Expected annotation count 0 for both designs.

### H02 — capability-free value

- **From**: `succeed(..)` in axum 010/050.
- **R**: `{}`. **E**: `Never`. **In**: —
- **Do**: return `42u32`; run it twice: once on the empty env and once *inside* H01's env (embedded
  as a step of an `R = {Greeting}` effect).
- **Region**: the handler, both runs, and the embedding expression.
- **Probes**: empty-R inference (`HNil` vs `()`), and lifting an env-free effect into a non-empty env
  (A: `narrow`/`Embed<HNil>`; B: the `IntoBind<R> for Effect<_,_,()>` impl).

### H03 — real async step

- **From**: axum `020_async_effect.rs` (`Effect::new_async` + `tokio::time::sleep`).
- **R**: `{}`. **E**: `Never`. **In**: —
- **Do**: sleep 1 ms on the kernel's executor timer, then return `"async step finished"`.
- **Region**: the handler and the run.
- **Probes**: `from_async` ergonomics, a `Send` future, and the executor's real timer (no
  `thread::sleep`).

### H04 — capability bound inside `effect!`

- **From**: axum `020_capability_run_with.rs` (`Needs::<Counter>::need(r)` inside `effect!`).
- **R**: `{Counter}`. **E**: `Never`. **In**: —
- **Do**: inside the design's `effect!` (or, if the spike has no macro, its desugaring with a comment
  saying so), bind `~Counter` and return `format!("count={}", c.0)`; run with `Counter(7)` →
  `"count=7"`.
- **Region**: the handler and the run.
- **Probes**: how the macro anchors `R` (B expects a `|r: &mut R|`-style anchor; A expects none).

### H05 — inline effect handed to a generic runner

- **From**: axum `030_execute_handler.rs` (`execute(st, |env| succeed(format!(..)))`).
- **R**: `{Counter}`. **E**: `Never`. **In**: —
- **Do**: there is *no named handler fn*. Write a tiny generic runner in the region,
  `fn execute<R, F>(env, f: F) -> Out where F: FnOnce() -> Effect<String, Never, R>` (adapt the bounds to
  the design), and call it with an inline closure that reads `~Counter` → `"n=7"`.
- **Region**: the runner, the call, and the inline closure.
- **Probes**: Design A risk R3(b), an unannotated closure with no expected type (E0282), and B's
  generic-`R` anchoring. This shape is the main annotation-count discriminator.

### H06 — capability whose method is itself async, composed (not `run_blocking`)

- **From**: axum `040_app_host.rs` (`run_blocking(store.get("demo"), ())` inside the handler).
- **R**: `{Sessions}`. **E**: `Never`. **In**: —
- **Do**: `~Sessions.get("demo").await` → `"user=user-1"` when seeded, `"user=none"` when not. The
  session lookup is bound in the effect; no nested blocking run.
- **Region**: the handler and both runs.
- **Probes**: binding a service future inside an effect, and that the env borrow does not leak into
  the `'static` future (A's `Ctx` clone; B's env ownership).

### H07 — request input, handler as a value

- **From**: axum `050_tower_effect_service.rs` (`EffectService::new((), |_env, x: u32| ..)`).
- **R**: `{}`. **E**: `Never`. **In**: `u32`.
- **Do**: `x.saturating_add(1)`. Store the handler in a `struct Service<F> { f: F }` defined in the
  region, then call it with `41` → `42`.
- **Region**: the struct, the handler value, the construction and the call.
- **Probes**: handlers as `Fn(In) -> Effect<..>` values and the inference of their `R` when stored
  generically.

### H08 — shared mutable state, env reused across runs

- **From**: axum `051_tower_shared_counter.rs`.
- **R**: `{Hits}`. **E**: `Never`. **In**: `&'static str` label.
- **Do**: `format!("{label} #{}", ~Hits.bump())`; run twice on the **same** env value →
  `"req #1"`, `"req #2"`, and `Hits::get() == 2` afterwards.
- **Region**: the handler and both runs.
- **Probes**: that running does not consume the env (env is shared or cheaply cloned), and that the
  `Arc` state is visible across runs.

### H09 — request-scoped override of an existing capability

- **From**: axum `060_request_caps.rs` (`CapOverrideLayer` replacing `Cap<Tenant>` per request).
- **R**: `{Tenant}`. **E**: `Never`. **In**: `Req`.
- **Do**: base env `Tenant("public")`. The edge reads the `x-tenant` header; if present it overrides
  `Tenant` for this request only. Handler returns `format!("tenant={}", ~Tenant.0)`. Run with and
  without the header → `"tenant=acme"`, `"tenant=public"`; the base env is unchanged afterwards.
- **Region**: the handler, the override at the edge, and both runs.
- **Probes**: A's `provide` on a sub-effect versus B's `provide_service` + `Replace`.

### H10 — request-scoped *addition* of a capability

- **From**: axum `060_request_caps.rs` plus the showcase's auth extractor (B §2.3 "`RequestEnv`").
- **R**: `{Tenant, Logger, CurrentUser}`. **E**: `AppError`. **In**: `Req`.
- **Do**: the base app env holds `{Tenant, Logger}` but **not** `CurrentUser`. The edge builds
  `CurrentUser { id }` from the `x-user` header (missing header → `AppError::Unauthorized`, without
  running the handler) and adds it for this request. The handler logs and returns
  `format!("{}@{}", user.id, tenant.0)`.
- **Region**: the handler, every env/struct declaration the addition needs (B's extension struct and
  its forward list go **inside** the region), and both runs.
- **Probes**: B risk R1 (no generic `R ∪ T`): count the forward list. A's `provide_some`/`Sculptor`.

### H11 — sql count with a typed error

- **From**: sql `010_axum_sql.rs` (`sql.query("SELECT COUNT(*) FROM users")`).
- **R**: `{Db}`. **E**: `DbError`. **In**: —
- **Do**: `~Db.count("users").await` → `n.to_string()`. Run on `Db::with_rows("users", 3)` → `"3"`,
  and on a failing db → `Err(DbError("unavailable"))`.
- **Region**: the handler and both runs.
- **Probes**: a fallible async service call with the typed `E` flowing out unchanged.

### H12 — transaction with rollback finalizer

- **From**: sql transactions (`id_effect_sql` `transaction`), `acquire_release` in 0.6.
- **R**: `{Db, Logger}`. **E**: `DbError`. **In**: `Vec<&'static str>` rows.
- **Do**: `begin`, insert every row, `commit`. If any insert fails (`"boom"`), the transaction is rolled
  back by a finalizer/`ensuring` (not by an explicit `match` in the body) and the error is returned.
  Log `"tx committed"` or `"tx rolled back"`. Runs: `["a","b"]` commits 2 rows; `["a","boom"]`
  fails, commits nothing, `Db::rollbacks() == 1`.
- **Region**: the handler and both runs.
- **Probes**: a finalizer that needs a capability-derived value (`Tx`), and `R` across
  `acquire_release`/`ensuring`.

### H13 — config read

- **From**: config `003_figment_provider_layer.rs` (`Config::string("app_name").run(..)`).
- **R**: `{ConfigSource}`. **E**: `ConfigError`. **In**: —
- **Do**: `~ConfigSource.get_string("app_name")` → `"example"`; missing key →
  `Err(ConfigError::Missing("app_name"))`.
- **Region**: the handler and both runs.
- **Probes**: a sync fallible service call lifted into the effect's error channel.

### H14 — nested override within one effect

- **From**: config `041_scoped_config_provider.rs` (`Env::scoped([provide_config_provider(..)])`).
- **R**: `{ConfigSource}`. **E**: `ConfigError`. **In**: —
- **Do**: outer env has `REGION=us-east`. The handler reads `REGION`, then runs a **sub-effect** that
  reads `REGION` under a locally provided `ConfigSource` (`REGION=eu-west`), then reads `REGION` again
  outside it → `"us-east/eu-west/us-east"`.
- **Region**: the handler and the run.
- **Probes**: lexical (not edge) provision; that the override does not leak out of the sub-effect.

### H15 — three capabilities and an error union

- **From**: config + sql + logger, as combined in the showcase.
- **R**: `{ConfigSource, Db, Logger}`. **E**: `AppError`. **In**: —
- **Do**: read key `table` from config, `count` that table, log `"counted {n}"`, return `n`. A missing
  key surfaces as `AppError::Config`, a db failure as `AppError::Db` (map/`From`, not a hand-written
  match per call).
- **Region**: the handler and three runs (ok, missing key, failing db).
- **Probes**: a 3-cap `R` and error widening; neither design may need index or `R` annotations here.

### H16 — composition with subsumption (narrowing)

- **From**: helpers called from handlers (the `~sub()` pattern, `CapBind` in 0.6).
- **R**: outer `{Db, Logger, Greeting}`. **E**: `DbError`. **In**: —
- **Do**: the outer handler calls three helpers with *smaller* environments, declared in a different
  order from the outer one: `log_line(msg)` needs `{Logger}`, H11's `count_users` needs `{Db}`, and
  H01's `hello` needs `{Greeting}`. It returns `format!("{greeting}:{n}")` and logs once.
- **Region**: the outer handler, the `log_line` helper, and the run. (H11/H01 are referenced, not
  copied.)
- **Probes**: A's `narrow`/`Embed` and order sensitivity R3(d); B's bound entailment. Any explicit
  `narrow::<..>()` or index is counted.

### H17 — one generic helper used from two different envs

- **From**: shared audit/logging helpers in services.
- **R**: helper `{Logger}`; caller 1 `{Logger, Db}`; caller 2 `{Logger, Mailer}`. **E**: `AppError`.
  **In**: —
- **Do**: write `audit(msg)` **once**, generic over the caller's environment, and call it from two
  handlers whose `R` differ (one counts users, one sends a mail). Both logs end up in the same
  `Logger`.
- **Region**: `audit`, both callers, and both runs.
- **Probes**: Design A risk R3(a), the index type leaking into every generic helper signature.

### H18 — recover from a typed error using another capability

- **From**: read-through cache fallbacks (resilience / sql).
- **R**: `{Db, Cache}`. **E**: `Never`. **In**: —
- **Do**: `count("users")`; on `DbError` fall back to `~Cache.get("users_count")` (default `"0"`).
  With a healthy db → `"3"`; with a failing db and a seeded cache → `"cached:7"`.
- **Region**: the handler and both runs.
- **Probes**: `catch`/`fold` where both branches use capabilities, and `E` changing to `Never`.

### H19 — parallel fan-out

- **From**: `zip_par`/`fork` + `join` (0.6 `fiber_all`).
- **R**: `{Db, Mailer}`. **E**: `AppError`. **In**: —
- **Do**: run `count("users")` and `send("ops@example.com", "report")` **concurrently** (the kernel's
  `zip_par`, or `fork` + `join` if the spike has no `zip_par`) → `(3, ())`; a failing db fails the
  whole effect with `AppError::Db`.
- **Region**: the handler and both runs.
- **Probes**: `R` crossing a fork boundary, `Send` children, and error propagation from a child.

### H20 — timeout around a hanging call

- **From**: per-request timeouts (`id_effect_resilience`, the showcase's per-request timeout).
- **R**: `{Db}`. **E**: `AppError`. **In**: `bool` hang.
- **Do**: race the db call against a 50 ms timeout. `hang = false` → `count` → `Ok(3)`;
  `hang = true` → `Db::hang()` is interrupted/dropped and the result is `Err(AppError::Timeout)` within
  well under a second.
- **Region**: the handler and both runs.
- **Probes**: interruption of an in-flight future, the executor timer, and `E` widening at the
  timeout.

### W01 — edge wiring of the showcase-sized app env

- **From**: `HostBuilder`/`bootstrap_env` (axum 040) and `build_env([provide!(..)])`; the D10
  showcase.
- **R** of the app: `{Greeting, Counter, Hits, Tenant, Logger, Sessions, Db, ConfigSource, Mailer,
  Cache}` (10 caps). **E**: `AppError`.
- **Do**: build the app env **in strata** with the design's Layer (or, if the spike has no `Layer`,
  plain constructor functions composed the same way): base `{ConfigSource, Logger}` → infra
  `{Db, Sessions, Cache}` where `Db`'s constructor reads `db_url` from `ConfigSource` →
  app `{Greeting, Counter, Hits, Tenant, Mailer}`. Then run H01, H11, H15, H16 and both H17 callers
  against that one env, unchanged.
- **Region**: every env struct/type, layer/constructor and merge, and the runs.
- **Probes**: A falsifier "showcase wiring needs a manual `I` or hits E0283"; B falsifier "more than one
  extra env struct per app, or forward lists above ~5 entries". Report B's forward-list total here.
