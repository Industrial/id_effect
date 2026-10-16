# release-0-9 — execution overlay

Mission `pln-mvbdsaam-th3g19` · spec `.maestro/specs/releases/0.9.0.md` (approved 2026-10-16) ·
branch `release/0.9.0` (off `release/0.8.0`; PR #105 carries 0.8.0, so 0.9 commits stay off it).

## Wave 1

Wave 1 starts in two steps: 1a needs no new crate and no `SqlValue`; 1b waits for the crate
skeletons (T0) and for the owner-visible `SqlValue` design review.

### 1a (dispatched at kickoff)

| Agent | Leaf (task id) | Files (owned) |
|---|---|---|
| A0 governance | `r09-t0-adr-skeletons` `tsk-mvbdsp4f-xmojqz` | docs/adrs/0010-crate-tiers.md (Amendment 5), root Cargo.toml members, crates/id_effect_{auth,authz,test}/ skeletons, scripts/validate-crate-packaging.sh, .github/workflows/{publish,ci}.yml |
| A1 sql design | `r09-e3-1a-values-rows` `tsk-mvbdsp4f-wd3fai` — **design note only** | .maestro/specs/releases/0.9/e3-1a-sql-values-design.md (no source) |
| A2 routes | `r09-e4-1-route-descriptor` `tsk-mvbdsp4f-ex9rcm` | crates/id_effect_axum/src/route/**, id_effect_axum Cargo.toml + lib.rs (own lines) |
| A3 profiles | `r09-e5-4-profiles` `tsk-mvbdsp4f-p6l6nv` | crates/id_effect_config/src/profile*.rs (new) + lib.rs lines, config tests |

### 1b (after A0 merges and the design note is approved)

| Leaf (task id) | Depends on |
|---|---|
| `r09-e3-1a-values-rows` `tsk-mvbdsp4f-wd3fai` (implementation) | design note approved |
| `r09-e3-1a-from-row` `tsk-mvbdsp4f-e7yo8j`, `r09-e3-1a-tx-stream` `tsk-mvbdsp4f-spcif4` | values-rows |
| `r09-e3-1a-ripple-docs` `tsk-mvbdsp4f-ua6x6x` | values-rows; tell the session that owns examples/http-showcase first |
| `r09-e3-2-migrate-api` `tsk-mvbdsp4f-a1hau9`, `r09-e3-2-cli` `tsk-mvbdsp4f-2m3dmu`, `r09-e3-2-docs` `tsk-mvbdsp4f-0yq2ci` | values-rows manifest merged |
| `r09-e4-1-extractors-problem` `tsk-mvbdsp4f-cbg990`, `r09-e4-1-docs` `tsk-mvbdsp4f-y1xx0b` | route-descriptor |
| `r09-e5-2a-passwords-sessions` `tsk-mvbdsp4f-c6kk1f`, `r09-e5-2a-credentials-jwt` `tsk-mvbdsp4f-i8nl5p` | A0 (id_effect_auth skeleton) |
| `r09-e5-3-policies` `tsk-mvbdsp4f-bcinb5`, `r09-e5-3-guards-testing` `tsk-mvbdsp4f-5hrfbn` | A0 (id_effect_authz skeleton) |
| `r09-e5-4-vault` `tsk-mvbdsp4f-souo6g` | profiles |

Rules (as 0.8): agents never commit or stage; the main session reviews and commits each leaf
pathspec-limited. Integrator-only files: CHANGELOG.md, docs/MIGRATION-0.9.md, book SUMMARY.md,
docs/scorecard/. examples/http-showcase belongs to a concurrent session and is off-limits until the
ripple leaf is announced.

Exit check (from A0): `publish.yml` publishes every crate in `validate-crate-packaging.sh` order
at the tag. Before tagging 0.9.0, each of id_effect_auth, id_effect_authz and id_effect_test must
have shipped leaves, or it comes out of the publish order (D8: no placeholders).

Follow-ups recorded during wave 1:

- `PgSessionStore: id_effect_auth::SessionStore` (+ migration, store SHA-256 of the session id) after
  `r09-e3-1a-values-rows`; completes sec.authn #2 (persistent store).
- E3.4 `RedisSessionStore` implements `id_effect_auth::SessionStore`, not the older
  `id_effect_platform::auth::session` trait.
- `r09-e5-2a-credentials-jwt` starts after `r09-e5-3-guards-testing`, which defines how the
  principal reaches request extensions.
- Axum route owner (after `r09-e4-1-extractors-problem`): add `Endpoint::layer` / `from_parts` so
  `.authorize()` returns an `Endpoint` (today `AuthorizedEndpoint::mount`), and a
  `RouteInfo.policies` field so OpenAPI and `/_routes` show the guarding policy; switch the authz
  guard's self-contained Problem body to `id_effect_axum`'s `Problem` type.
- Gates under concurrent agents: set `TMPDIR` on `/data` (the 2 GB `/tmp` tmpfs fills and crashes
  llvm-cov / lld).
- id_effect_sql: plain `PgSqlClient::query`/`execute` errors drop the SQLSTATE (only transaction
  and stream errors carry it); add a typed `SqlError` code instead of parsing the message.
  `SqlTransaction` has no `query_stream` yet.
- `#[derive(Schema)]` stops at the first error, so `#[from_row(schema)]` on a derived schema reports
  one issue per row; combinator schemas report all (fix in `derive_schema_parser.rs`).
- `.config/nextest.toml`: if trybuild runs collide, serialise `package(id_effect_sql) &
  binary(from_row)` like the core ui tests.
- Commit rule learned the hard way (fixed by 2nd commit after 2905241): `git add` before every
  pathspec commit, otherwise new files are silently left out; check `git status` for `??` after.
- **Security gap (before 0.9 exit):** id_effect_sql builds sqlx without a TLS feature, so
  PostgreSQL and MySQL connections are plaintext. Add rustls TLS (`sqlx/tls-rustls-aws-lc-rs` or
  ring, matching the tree) behind the driver features, with `sslmode` / `ssl-mode` honoured from
  the URL, and a test against a TLS-enabled container.
- **Coverage gate (before 0.9 exit):** run `scripts/llvm-cov-nextest.sh` (workspace, 96 % floor)
  once the tree is quiet; core `id_effect` measured alone at ~94 % during wave 2. Raise it or
  carry the gap in writing.
- Agent rule: never put the owner's email (or any personal data) in requests to external
  services; use a generic User-Agent such as `id_effect-release-check (+repo URL)`. An earlier
  crates.io name check sent it in the User-Agent (fixed in `.tmp/cratesio.sh`).
- `RedisSessionStore` Lua scripts are not Redis-Cluster safe (no hash tags); documented.
- `src/metrics.rs` doc links to `record_pg_pool` break `cargo doc -D warnings` without `pg`.
- `verify_is_constant_time_on_unknown_user` compares timing medians (0.33–3×); watch for flakes on CI.

## Waves 2–3

Tasks are created with `task from-spec` when their wave starts (the mission can only be decomposed
once); see the spec's Waves table.

## Log

| Date | Event |
|---|---|
| 2026-10-16 | Spec approved (D1 (b), D2–D12 as recommended). Mission created, wave 1 decomposed (17 leaves). Wave 1a dispatched. |
| 2026-10-16 | Wave 1: 16 of 17 leaves shipped plus PgSessionStore (`tsk-mvbg796n-xpvbrc`); `r09-e3-2-docs` running. SqlValue design reviewed and approved by the owner before driver work. |
| 2026-10-16 | Wave 2: 17 leaf tasks created from `.maestro/specs/releases/0.9/r09-*.md`. Batch A dispatched: sqlite + conformance-routing, sea-query, openapi-doc (+ Endpoint::layer, RouteInfo.policies), middleware layers, OIDC, containers. Next: mysql, repository, static-cache, ui-routes, totp, audit, tenancy-doc, isolation-fixtures, e3-3-docs; E3.4 Redis stretch. |
