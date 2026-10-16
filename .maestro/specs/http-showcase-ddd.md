---
title: Rewrite examples/http-showcase in layer-first DDD, one construct per file
slug: http-showcase-ddd
mode: light
work_type: change-request
risk_class: medium
version: 1
acceptance_criteria:
  - 'src/ is layer-first: domain/, application/, infrastructure/, interface/http/, bootstrap/; lib.rs and every mod.rs only declare modules and re-export.'
  - 'One construct per file: every struct, enum, trait, type alias, free function (use case, handler, layer constructor) has its own file; a type''s inherent impls live with it; unit tests stay in a #[cfg(test)] block of the file they test; test doubles live in infrastructure/doubles/ under cfg(test).'
  - 'Domain: TodoId and Title are brands/value objects; Todo holds a Title (not String) and owns its behaviour (rename, complete, reopen, apply(TodoChanges)); domain types derive EffectData where they qualify; no axum/sql types in domain/.'
  - 'Errors are typed per use case (Or<..> of domain errors and the store error); HTTP status mapping lives only in interface/http; SqlError mapping lives only in infrastructure/postgres.'
  - 'Request bodies decode through #[derive(Schema)], reporting every bad field in one 422; the wire format and every status code of the README tables are unchanged.'
  - 'Store selection is layer composition (in-memory vs scoped Postgres provider that migrates on acquire), not a hand-written match; id_effect_resilience CircuitBreaker wraps store calls alongside the Schedule retry + timeout; id_effect_logger emits a structured startup line (bind address, store kind).'
  - 'Public API kept as re-exports used by tests/http_pg.rs and the README, or the callers updated in the same change.'
  - 'Gates green: cargo clippy -p http_showcase --all-targets --all-features -- -D warnings; #![deny(missing_docs)]; cargo nextest run -p http_showcase with DATABASE_URL against devenv Postgres and ID_EFFECT_REQUIRE_PG=1; cargo test --doc -p http_showcase.'
non_goals:
  - Changing id_effect library crates (findings there become follow-up tasks).
  - Fixing the SQLSTATE-less unique-violation detection in id_effect_sql.
  - An Fsm for todo completion.
---

# http-showcase DDD rewrite

User decisions (2026-10-15): layer-first layout; per-use-case error types (`Or<..>`); add
resilience (circuit breaker) and the logger, skip the FSM; one file per free function too;
plan auto-approved, execute with full concurrency and subagents.

Waves:
1. Domain + ports + cross-layer contracts (serial, main session).
2. Parallel subagents on disjoint paths: application | infrastructure | interface/http.
3. Bootstrap (layers, startup error, run), lib.rs re-exports, main.rs, tests/http_pg.rs, README.
4. Gates + review.
