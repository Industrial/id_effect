# release-0-10 — execution overlay

Mission `pln-mvcjuor2-9k8v3r` · spec `.maestro/specs/releases/0.10.0.md` (approved 2026-10-17) ·
branch `release/0.10.0` (off `main` at 53a140c, after the 0.9.0 release and #107).

## Wave 1

At most five agents build at once (0.9 lesson: a loaded machine times out SQLite/MySQL container
tests and skews compile-time KPIs). Batch 1a needs no new crate; batch 1b waits for the crate
skeletons from `r10-t0-adr-skeletons`.

### 1a (dispatched at kickoff)

| Agent | Leaf (task id) | Files (owned) |
|---|---|---|
| A0 governance | `r10-t0-adr-skeletons` `tsk-mvcjv4jp-snazrp` | docs/adrs/0010-crate-tiers.md (Amendment 6), root Cargo.toml members, crates/id_effect_{jobs,messaging,mail,console,storage}/ skeletons, scripts/validate-crate-packaging.sh, .github/workflows/ci.yml (-p lines), deny.toml, docs/scorecard/scorecard.json (sec.authn 0.10.0 target only) |
| A1 sql wire | `r10-sqlt-pg-wire` `tsk-mvcjv4gc-nokr7e` | crates/id_effect_sql/src/pg/{value.rs,datetime.rs}, src/value/time_conv.rs |
| A2 schema errors | `r10-co-schema-errors` `tsk-mvcjv2xv-k2omgn` | crates/id_effect_proc_macro/src/derive_schema_parser.rs (+ its tests / ui snapshots) |
| A3 logging | `r10-e6-4-redaction-sampling` `tsk-mvcjv3m4-28cr81` | crates/id_effect_logger/** |

### 1b (after A0 merges; then as slots free up)

| Leaf (task id) | Depends on / note |
|---|---|
| `r10-e7-1-queue-core` `tsk-mvcjv3wa-40y1j9` | A0 (jobs skeleton). **Design note first** (`.maestro/specs/releases/0.10/e7-1-queue-design.md`: `Job` trait, `JobQueue`, table + indexes, states, worker loop); reviewed before code, and before E7.3 / E1.3r start. Uses `r10-co-sql-errors`' SQLSTATE once merged. |
| `r10-co-sql-errors` `tsk-mvcjv31o-sevooz` | none (id_effect_sql error.rs, pg/error.rs, pg/client.rs; id_effect_node singleton.rs) |
| `r10-sqlt-mysql-query-ci` `tsk-mvcjv4cx-u59do2` | A1 (same crate Cargo.toml; sequential) |
| `r10-e4-3-client-defaults` `tsk-mvcjv35b-3d8syc` | none |
| `r10-e4-3-tracing` `tsk-mvcjv38n-qmey5i` | none (owns id_effect_sql src/metrics.rs, id_effect_opentelemetry) |
| `r10-e6-5b-faults` `tsk-mvcjv3pj-tef3al` → `r10-e6-5b-guide` `tsk-mvcjv3sy-7786hr` | none (id_effect_test fault/, TestSqlClient in id_effect_sql client.rs) |
| `r10-reload-config` `tsk-mvcjv466-ezgto8`, `r10-reload-rolling` `tsk-mvcjv49j-j19atg` | none |
| `r10-e7-4-promote` `tsk-mvcjv3zi-qxn0mq` → `r10-e7-4-snapshots-subscriptions` `tsk-mvcjv42t-2myvtg` | A0 (adds the id_effect_events member line itself) |
| `r10-e4-4-storage` `tsk-mvcjv3fd-36woc8` → `r10-e4-4-container-docs` `tsk-mvcjv3bz-jrd70e` | A0 (storage skeleton) |
| `r10-e6-2-fiber-registry` `tsk-mvcjv3iq-y7wzar` | none; bench gates (`docs/benchmarks/gates.toml`, `crates/id_effect/tests/kernel_alloc.rs`) green with the feature off and on |
| `r10-t0-k2-k6` `tsk-mvcjv4mz-pvmh4b` | run its measurement when no other agent is building (idle machine) |

## Wave 2 (tasks created after `r10-e7-1-queue-core` landed)

| Leaf (task id) | Depends on |
|---|---|
| `r10-e7-3-transports` `tsk-mvcpexut-0na9la` → `r10-e7-3-templates-preview` `tsk-mvcpexxx-4ohdhl` → `r10-e7-3-deliver-later` `tsk-mvcpey14-2k0wis` → `r10-auth8-accounts` `tsk-mvcpey4b-b89h8y` | queue-core (deliver-later); longest chain, start first |
| `r10-e7-2-broker-api` `tsk-mvcpexl4-f87btw` → `r10-e7-2-outbox-inbox` `tsk-mvcpexoh-4d52ro` → `r10-e7-2-redis-streams` `tsk-mvcpexrp-ffsx4n` | queue-core (outbox relay reuses the worker pattern) |
| `r10-e7-1-cron` `tsk-mvcpexet-7d5r2f` | queue-core; adds core `CronJob::new_at` (Q5) |
| `r10-e7-1-docs-labs` `tsk-mvcpexhy-k9wn2c` | outbox-inbox (msg.brokers#8 must exist before ObixInbox goes) |
| `r10-e6-2-console-api` `tsk-mvcpey7z-zm5f97` → `r10-e6-2-dashboard` `tsk-mvcpeyb9-t3b4bu` | fiber registry (done); jobs panel optional |
| `r10-e1-5-dev` `tsk-mvcpeyei-jtr1wv` → `r10-e1-5-devcontainer` `tsk-mvcpeyhp-jmyzba` → `r10-e1-3-generate-job` `tsk-mvcpeyky-peqniz` | share crates/id_effect_tool cli.rs / template.rs: sequential |

D1 checkpoint at the end of wave 2: if E1.9 (dylint) has not started, move dx.lint's 0.10.0
target to 0.11.0.

Rules (as 0.9): agents never commit or stage; the main session reviews each leaf and commits it
pathspec-limited (`git add` the leaf's paths first, then `git commit -F <file> -- <paths>`).
Integrator-only files: CHANGELOG.md, docs/MIGRATION-0.10.md (except the section draft owned by
`r10-sqlt-mysql-query-ci`), book SUMMARY.md, docs/scorecard/ (except A0's one target line).
Gates set `TMPDIR` under `/data` (the 2 GB `/tmp` tmpfs fills). Live tests use
`ID_EFFECT_REQUIRE_PG=1` / `ID_EFFECT_REQUIRE_DOCKER=1` and fail rather than skip. Never send the
owner's email or other personal data to external services.

## Follow-ups recorded during the release

- E7.1 design approved by the owner 2026-10-17 (`.maestro/specs/releases/0.10/e7-1-queue-design.md`:
  Q1 `scheduled` + `cancelled`, Q2-Q5 as recommended). queue-core may touch `src/lib.rs`,
  `Cargo.toml`, `tests/conformance.rs`, `tests/pg_worker.rs` beyond its Owns list (note's amendment).
  Q5 needs an additive core `CronJob::new_at` in `r10-e7-1-cron`.
- Integrator, CHANGELOG + MIGRATION-0.10 from wave 1 commits:
  - see each commit body: e64163a (logger), 803151c (schema), f52e67ec + the SQL pair
    (sqlx time, SqlError variants / sqlstate / 23505, PgPoolConfig::connect_options public,
    MySQL date-time params bind as text), the HTTP client defaults (30 s timeout, retries; HEAD /
    OPTIONS not in `HttpMethod`), storage, fiber registry, 7b99a566 (quinn on aws-lc-rs).
- HttpMethod lacks HEAD / OPTIONS (D10 lists them as retryable): adding variants is breaking;
  decide before the 0.10 migration guide is final.
- `ReqwestClientLive::layer()` / `provide_reqwest_pool` still build a bare `Client::new()` (no
  timeout); `HttpError::clone` turns `Reqwest(e)` into `InvalidRequest`.
- id_effect_storage: an interrupted multipart upload is not aborted (only failures are).
- `examples/http-showcase/.../unique_violation.rs` (and maybe the Ledger) can use
  `SqlError::is_unique_violation()` instead of matching message text.
- Security (pre-existing in `Profiles::load`, now also in `ProfileWatch`): figment's TOML errors
  quote the broken source line and bad values, so a malformed line holding a secret can reach a
  `ConfigError` message / `last_error()`. Redact config error text (keep path + kind) — small
  fix in id_effect_config; schedule before exit.
- Events (from the promote commit): book/docs still say labs or es-entity in
  `part6/ch31-00-async-messaging.md` (:4 :11 :59 :71; e7-1-docs-labs rewrites it),
  `part6/ch26-00-platform-introduction.md:44`, `appendix-a-api-reference.md:185`,
  `appendix-c-glossary.md:173`, `docs/platform/PARITY_DASHBOARD.md:20`,
  `docs/platform/adrs/adr-es-entity-event-persistence.md` (supersede note). `labs/id_effect_graph`
  has no consumer left (remove or keep, decide in e7-1-docs-labs). `dispatch_command` appends
  without an expected version (handlers never see state); conflicts are caught only when two
  appends race. MIGRATION-0.10 lines in the commit body.
- Tracing (from the E4.3 tracing commit): book_snippets builds id_effect_opentelemetry without
  `platform`, so the `TracedHttpClient` snippet in ch27-00 is `rust,ignore`; enable the feature in
  `examples/book-snippets/Cargo.toml` (K5). `ch30-04-credentials-jwt` doctests take > 60 s
  (argon2 cost?) — check. `cargo clippy -p id_effect_opentelemetry --no-default-features` fails
  on existing code (`install_from_config` re-export, unused `OtelError` import).
- Service template (from the readiness fix): the generated main no longer installs the OTel span
  hook on the root fiber; wire request spans with `RequestIdLayer::span_hook(telemetry.span_hook())`
  in `r10-w3-kit-template` (service-pg-otel variant). `server_layer_with_readiness` is only
  exported from `server::lifecycle`.
- FIXED (c75f9f0d, 9a9a498a, 3e9cec38): the singleton/lease bugs below, figment error redaction,
  derive(Schema) hygiene + single unsupported-field error. New from those fixes: derive(Schema)
  uses `field_ident.to_string()` as the wire key, so `r#type` is sent as "r#type" (should be
  "type"); `Profiles::load_for` TOML parse errors don't name the file.
- Bugs, id_effect_node (pre-existing, found by r10-e7-1-cron; fixed in c75f9f0d):
  `ClusterSingleton::resign` clears `local_pid` only when release succeeds, so after a lost lease
  (`HeldBy`) it keeps the old pid; `PgAdvisoryLease::try_acquire` treats its in-memory `held` map
  as ownership without pinging, so after its session died the same holder "re-acquires". Also:
  console nested supervisors need a child kind from `which_children` (id_effect_process);
  `logs_bridge.rs` subscriber builders still use a non-reloadable filter;
  `tier-conformance.sh id_effect_opentelemetry` fails on 15 pre-existing "stub" markers.
- DX (from dev / devcontainer): `id-effect dev --services` runs `docker compose up -d` on every
  service, incl. the project's own release container (takes :8080 for service) - add a service
  filter (e.g. only services with a `profiles: [dev]` or `--services db,redis`). Template READMEs
  don't mention `.devcontainer/`. Windows path of `id-effect dev` not compile-checked.
- D1 checkpoint applied 2026-10-17 (dx.lint target 0.10.0 -> 0.11.0; planned 0.10.0 E 73.8 #2,
  RT 76.8 #1, red 12). TODO: add its PLAN-OF-PLANS §11 row once the other session's uncommitted
  PLAN-OF-PLANS edit (L14: 1.0 postponed, 0.13 / 0.14 inserted) is committed - do not mix them.
- KPIs (from 07f02ce9): K6 budget 120 s is a proposal - owner to confirm at exit. r10-w3-kpis:
  run idle without CARGO_BUILD_JOBS (it caps K6), re-measure K1 idle, K3 with the job (emit_json
  K3 strings still say "none at 0.9"), update moon.yml:205 dx-kpis description (still "0.9",
  K1/K3/K4/K5 only). PLAN-OF-PLANS:336 names docs/scorecard/results/<ver>.dx.json but the script
  writes docs/benchmarks/dx-kpis-<release>.json. Accounts: moon.yml of id_effect_auth / mail
  lack migrations/** and templates/** inputs; ch30-03 is not doctested.
- Hardening batch (A: logger process-wide redaction + console CSRF + csrf_exempt_bearer + OTel
  layers; B: InstrumentedSqlClient forwarding, SqlClient for Arc/Box, Inbox::handler, storage
  Bytes, retry_now for failed; C: CI pg coverage (H1) + container port-race retry, 940b229c).
  Leftover: a stray Maestro evidence record evd-1792271996399-ca0655 against a nonexistent task
  tsk-000000 (placeholder, created by mistake; cannot be deleted with the tools). Logger minimum
  level / annotations / spans are still per-thread (functional gap, 0.11). The console's
  __Host-csrf cookie is Secure (needs HTTPS or localhost).
- Index hygiene: something (likely a Maestro command) staged ~330 files incl. other agents' WIP
  during wave 1. Pathspec commits are unaffected; never run a bare `git commit`.
- Fiber registry bench numbers came from a loaded machine: re-measure idle in `r10-w3-kpis`.
  `crates/id_effect/src/lib.rs` feature list does not mention `fiber-registry` yet.

- Integrator, CHANGELOG + MIGRATION-0.10 (from e64163a, E6.4): `RedactionPolicy` /
  `provide_redaction_policy`, `SamplingBackend` / `SamplingStats`, `LogRecord.target` +
  `EffectLogger::log_target`; **breaking** `LogRecord` is `#[non_exhaustive]` (build with
  `LogRecord::new` + `with_*`); output gains `target` only when set.
- Integrator (from 803151c, co-schema-errors): CHANGELOG "derived schemas report every invalid
  field; `Schema::with_decode_unknown_all` public"; MIGRATION note: `#[from_row(schema)]` on a
  derived type now lists every invalid column in `SqlError::InvalidRow { issues }`.
- Bug (pre-existing, not yet fixed): `#[derive(Schema)]` generated code is not hygienic — a field
  named `obj` (or now `__issues`) shadows the decoder's locals
  (`crates/id_effect_proc_macro/src/derive_schema_parser.rs`); rename locals to `__id_effect_*`
  and add a test with such fields. Also the "unsupported field type" error is emitted 3×.
- id_effect_sql (pre-existing, seen in f52e67ec): sqlx caches prepared statements by SQL text
  with their parameter types, so the same SQL bound first with a `Timestamp` and later a
  `DateTime` reuses the TIMESTAMPTZ statement. Document in the SQL values chapter (or key the
  cache by types). Also: the local Postgres on :5432 was down during wave 1; agents use their own
  `postgres:17` containers on 5543x ports.
- Spec fix: the E6.4 row names `book/src/part6/ch27-00-observability.md` (does not exist); the
  section went to `part2/ch07-11-logger.md`.
- Note: the redaction policy is installed per thread like the composite backend
  (`id_effect_logger/src/lib.rs` thread-locals); the console's runtime log level (E6.2 #6) needs a
  process-wide handle anyway — consider moving both together there.
