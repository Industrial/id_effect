---
title: '0.13 follow-ups (F53) — Postgres and Redis idempotency stores for RPC mutations: r14-f-idempotency-pg'
slug: r14-f-idempotency-pg
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F53: two shared implementations of `id_effect_rpc::idempotency::IdempotencyStore` (crates/id_effect_rpc/src/idempotency.rs:163; `begin` → `Begin::{Fresh,Replay,Mismatch,InProgress}` :147, `complete`, `abandon`), beside the bounded `MemoryIdempotencyStore` (:195), so a retried mutation replays on every instance: `PgIdempotencyStore` (feature `idempotency-pg`, on any id_effect_sql `SqlClient`; table `id_effect_rpc_idempotency` keyed (scope, tag, key) with digest, status, body, created_at, shipped as `idempotency::pg::migrator()` with its own tracking table, as id_effect_jobs does in crates/id_effect_jobs/src/pg.rs:113) and `RedisIdempotencyStore` (feature `idempotency-redis`, on id_effect_redis `RedisClient`; reservation by `SET NX PX`, digest compare and completion in one Lua script, namespace required and non-empty). Both plug in through `RpcServer::with_idempotency_store` (crates/id_effect_rpc/src/server/mod.rs:197) unchanged. The Ledger''s app-local store (examples/reference/ledger/src/idempotency.rs, migration 0010_rpc_idempotency) is the design source; switching the Ledger to the library store is left to r14-p-ledger.'
  - 'Placement (no dependency cycle): the stores live in id_effect_rpc as features `idempotency-pg = ["dep:id_effect_sql", "id_effect_sql/pg", "id_effect_sql/migrate"]` and `idempotency-redis = ["dep:id_effect_redis"]`. Features of id_effect_sql or id_effect_redis would need them to depend on id_effect_rpc, and id_effect_rpc → id_effect_events (feature `events`, crates/id_effect_rpc/Cargo.toml:95) → id_effect_sql (crates/id_effect_events/Cargo.toml:28) makes sql → rpc a cycle; id_effect_redis and its optional dependencies (auth, axum, messaging) never depend on id_effect_rpc, so rpc → redis is acyclic. `cargo check -p id_effect_rpc --all-features` resolves (cargo rejects a cyclic package graph).'
  - 'Tests: crates/id_effect_rpc/tests/idempotency_stores.rs, one conformance body run against memory, Postgres and Redis: `store_reserves_replays_and_refuses_mismatches_{memory,pg,redis}`, `store_reports_in_progress_while_the_first_call_runs_{pg,redis}`, `abandon_releases_the_key_{pg,redis}`, `expired_keys_are_fresh_again_{pg,redis}`, `two_servers_share_one_store_and_replay_{pg,redis}` (two `RpcServer`s, one retried mutation runs once), `keys_are_scoped_per_caller_{pg,redis}`; Postgres and Redis containers through id_effect_test (`Postgres`, `Redis`), `ID_EFFECT_REQUIRE_DOCKER=1`.'
  - 'Book: the idempotency paragraph of `crates/id_effect/book/src/part6/ch33-01-procedures.md` (:131-136, which today names only `MemoryIdempotencyStore` and "the seam") becomes a subsection "### Shared stores" (Postgres, Redis, migration, TTL); example `crates/id_effect_rpc/examples/030_idempotency_store.rs` (required-features `idempotency-pg`).'
  - 'Tier: platform (id_effect_rpc). Secure by default (ADR 0013): R4 bounded: TTL (default 24 h, as the memory store) enforced by expiry on both stores, stored body capped (default 1 MiB; a larger result is answered but not stored, and the key abandoned), keys validated by `parse_key` (:114) before any store call; R5 only the crate''s own parameterised SQL, and the Redis namespace may not be empty (as `m17_clear_with_empty_namespace_is_an_error`, crates/id_effect_redis/tests/store_clear.rs:15); keys stay scoped per caller (`IdempotencyScope`, :63) so one principal cannot replay another''s result.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_rpc --all-features --summary-only --fail-under-regions 96` (with Docker).'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_rpc --all-features --test idempotency_stores`; `cargo nextest run -p id_effect_rpc --all-features`; `cargo clippy -p id_effect_rpc --all-targets --all-features -- -D warnings`; `bash scripts/tier-conformance.sh id_effect_rpc`; CI job `rpc` (ci.yml:445) runs the container tests (its `env:` gains `ID_EFFECT_REQUIRE_DOCKER: ''1''`)'
  - 'Owns: `crates/id_effect_rpc/src/idempotency/{pg.rs,redis.rs}` (new; `src/idempotency.rs` becomes `src/idempotency/mod.rs` by `git mv` with only the two `mod` lines added), `crates/id_effect_rpc/migrations/**` (new), `crates/id_effect_rpc/tests/idempotency_stores.rs` (new), `crates/id_effect_rpc/examples/030_idempotency_store.rs` (new), `crates/id_effect_rpc/Cargo.toml` (features `idempotency-pg`, `idempotency-redis`, the optional id_effect_sql and id_effect_redis dependencies, dev-dependency features of id_effect_test, `[[example]]` entry), `crates/id_effect/book/src/part6/ch33-01-procedures.md` (the idempotency subsection), the kit feature lines `rpc-idempotency-pg` and `rpc-idempotency-redis` in `crates/id_effect_kit/Cargo.toml`, `.github/workflows/ci.yml` (the `env:` block of job `rpc`); append-only: `crates/id_effect_kit/src/lib.rs` (the two feature-table rows), `CHANGELOG.md`, `Cargo.lock`'
---

# r14-f-idempotency-pg

Leaf of `.maestro/specs/releases/0.14.0.md`, section "0.13 follow-ups". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: the wave-1 merge (no wave-1 leaf owns its files). Blocks r14-p-ledger (wave 4),
which may replace examples/reference/ledger/src/idempotency.rs with `PgIdempotencyStore` (its table
name differs: a data migration or keeping the app store is that leaf's call).

Defaults the owner may overturn: the stores are features of id_effect_rpc (cycle argument above); the
library table is `id_effect_rpc_idempotency`, not the Ledger's `rpc_idempotency`; expiry is lazy (on
`begin`, per scope, as the Ledger does) plus Redis key TTLs, with no background sweeper. Not done here:
a MySQL or SQLite store, changing the `IdempotencyStore` trait.
