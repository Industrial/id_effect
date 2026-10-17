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
