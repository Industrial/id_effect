# id_effect

**id_effect** is a typed effect runtime for Rust: `Effect<A, E, R>`, capability-based dependency injection, structured concurrency, resilience, pull-based streams and a first-class testing kit — plus OTP-grade supervision and distribution for **long-running supervised services, daemons and CLIs**. The design is inspired by [Effect](https://effect.website) (Effect-TS) and Erlang/OTP: programs describe work as lazy values, wire dependencies through the type system, run under an explicit runtime, and recover under supervision.

It is growing into a batteries-included platform ([ADR 0012](docs/adrs/0012-platform-vision.md)): the compiler checks your dependencies, errors and resources, the runtime supervises them, and one command gets you a production-shaped service. Today it ships the runtime, supervision, and integrations for Axum and SQL; further effect-native batteries (migrations, auth, jobs, mail, storage, an in-app console) and the `id_effect_kit` facade and `id-effect` CLI are planned for the releases leading to 1.0.

<!-- Badge row 1: CI & quality -->
[![CI](https://github.com/Industrial/id_effect/actions/workflows/ci.yml/badge.svg)](https://github.com/Industrial/id_effect/actions/workflows/ci.yml)
[![Docs & Pages](https://github.com/Industrial/id_effect/actions/workflows/docs.yml/badge.svg)](https://github.com/Industrial/id_effect/actions/workflows/docs.yml)
[![Security Audit](https://github.com/Industrial/id_effect/actions/workflows/audit.yml/badge.svg)](https://github.com/Industrial/id_effect/actions/workflows/audit.yml)
[![codecov](https://codecov.io/gh/Industrial/id_effect/branch/main/graph/badge.svg)](https://codecov.io/gh/Industrial/id_effect)

<!-- Badge row 2: crates.io -->
[![crates.io](https://img.shields.io/crates/v/id_effect.svg)](https://crates.io/crates/id_effect)
[![docs.rs](https://docs.rs/id_effect/badge.svg)](https://docs.rs/id_effect)
[![downloads](https://img.shields.io/crates/d/id_effect.svg)](https://crates.io/crates/id_effect)

<!-- Badge row 3: repository health -->
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.94-orange.svg)](https://www.rust-lang.org)
[![Edition](https://img.shields.io/badge/edition-2024-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![GitHub Stars](https://img.shields.io/github/stars/Industrial/id_effect?style=social)](https://github.com/Industrial/id_effect/stargazers)

### Start with the book

**[Typed Effects in Rust](https://industrial.github.io/id_effect/)** is the main way to learn **id_effect**: from `Effect<A, E, R>` and the `effect!` macro, through capabilities and providers, to concurrency, resources, scheduling, streams, schema, testing and supervision. The API docs answer “what does this type do?”; the book answers “how do I think in effects?”.

- **Read online:** [Typed Effects in Rust](https://industrial.github.io/id_effect/) (GitHub Pages)
- **Terminology:** [Glossary](https://industrial.github.io/id_effect/appendix-c-glossary.html)
- **Upgrading:** [Migration guide to 0.7](docs/MIGRATION-0.7.md) (from 0.6), [to 0.6](docs/MIGRATION-0.6.md) (from 0.4 / 0.5), and the [CHANGELOG](CHANGELOG.md)
- **Stability:** [semver, deprecation and MSRV policy](docs/STABILITY.md)
- **Source:** [`crates/id_effect/book/`](crates/id_effect/book/) (build locally with `mdbook build crates/id_effect/book`)

---

## Overview

**id_effect** models programs as **descriptions** of work (`Effect` values) rather than immediate side effects. You compose them, attach requirements to the type signature, and run them when you choose:

- **`Effect<A, E, R>`** — success type `A`, error type `E`, requirements `R`.
- **Capabilities and providers** — typed dependency injection: declare what an effect needs (`caps!`, `need`), provide it once at the edge as a `Layer` (`#[derive(ProviderSpecDerive)]`, `provide_layer`).
- **`pipe!` and `effect!`** — composition and do-notation-style blocks without hiding what the types mean.
- **Structured concurrency and resilience** — fibers, scopes, cancellation, `Schedule`-driven retry, circuit breakers.
- **Pull-based streams** — bounded-memory, incremental operators that work on infinite sources.
- **Supervision and distribution** — `id_effect_process` (processes, links, monitors, supervision trees, gen_statem-style FSMs) and `id_effect_node` (Process Mesh across nodes).
- **Testing kit** — the deterministic `TestExecutor` (virtual clock: `run_until_stalled`, `advance`, `next_timer_deadline`), a seeded/fed `TestRandom`, and capability mocks provided as layers.

The core crate runs effects as fibers on a pluggable executor (a multi-thread `TokioExecutor` in production, the deterministic `TestExecutor` with a virtual clock in tests): `Runtime::run(eff).await` in async code, `Effect::block_on` / `Runtime::block_on` at a synchronous edge. The default `compute` feature adds the parallel Compute Fabric; `id_effect_tokio` is the Tokio runtime adapter.

---

## Crates in this workspace

<!-- crates:begin (generated by scripts/readme-crates.sh; do not edit) -->

**Core**

| Crate | Version | Description |
|-------|---------|-------------|
| [`id_effect`](crates/id_effect) | [![crates.io](https://img.shields.io/crates/v/id_effect.svg)](https://crates.io/crates/id_effect) | Effect<A, E, R>: lazy Send effects over a typed capability list, run on a fiber runtime (Tokio executor), with layers, streams, schedules and coordination |
| [`id_effect_config`](crates/id_effect_config) | [![crates.io](https://img.shields.io/crates/v/id_effect_config.svg)](https://crates.io/crates/id_effect_config) | Configuration for id_effect: Config descriptors, ConfigProvider and Figment/serde loading |
| [`id_effect_logger`](crates/id_effect_logger) | [![crates.io](https://img.shields.io/crates/v/id_effect_logger.svg)](https://crates.io/crates/id_effect_logger) | EffectLogger capability for id_effect: log levels, annotations and pluggable backends over tracing |
| [`id_effect_macro`](crates/id_effect_macro) | [![crates.io](https://img.shields.io/crates/v/id_effect_macro.svg)](https://crates.io/crates/id_effect_macro) | Declarative macros re-exported by id_effect: pipe!, err! |
| [`id_effect_proc_macro`](crates/id_effect_proc_macro) | [![crates.io](https://img.shields.io/crates/v/id_effect_proc_macro.svg)](https://crates.io/crates/id_effect_proc_macro) | Procedural `effect!` macro for the `effect` crate (proc-macro only; declarative macros live in `id_effect_macro`) |
| [`id_effect_resilience`](crates/id_effect_resilience) | [![crates.io](https://img.shields.io/crates/v/id_effect_resilience.svg)](https://crates.io/crates/id_effect_resilience) | Resilience patterns for id_effect: circuit breaker, Schedule-driven retry, rate limiter, bulkhead, hedged requests, fallback, policy pipelines |
| [`id_effect_tokio`](crates/id_effect_tokio) | [![crates.io](https://img.shields.io/crates/v/id_effect_tokio.svg)](https://crates.io/crates/id_effect_tokio) | Tokio runtime adapter for id_effect: TokioRuntime over the 0.7-kernel TokioExecutor (fibers as tokio tasks) |

**Runtime and supervision**

| Crate | Version | Description |
|-------|---------|-------------|
| [`id_effect_fsm`](crates/id_effect_fsm) | [![crates.io](https://img.shields.io/crates/v/id_effect_fsm.svg)](https://crates.io/crates/id_effect_fsm) | Typed finite-state machines with Effect-valued transitions, derive(Fsm), sagas, and a gen_statem-style process behaviour for id_effect |
| [`id_effect_node`](crates/id_effect_node) | [![crates.io](https://img.shields.io/crates/v/id_effect_node.svg)](https://crates.io/crates/id_effect_node) | Distribution layer for the id_effect Process Mesh: node identity, cookie handshake, and pluggable transports (InMemory/TCP/QUIC) |
| [`id_effect_process`](crates/id_effect_process) | [![crates.io](https://img.shields.io/crates/v/id_effect_process.svg)](https://crates.io/crates/id_effect_process) | BEAM/OTP-style processes, links, monitors, and supervision trees for id_effect |

**Integrations**

| Crate | Version | Description |
|-------|---------|-------------|
| [`id_effect_axum`](crates/id_effect_axum) | [![crates.io](https://img.shields.io/crates/v/id_effect_axum.svg)](https://crates.io/crates/id_effect_axum) | Axum and Tower integration for id_effect — Send-safe Effect handlers, request-scoped capabilities, host lifecycle |
| [`id_effect_cli`](crates/id_effect_cli) | [![crates.io](https://img.shields.io/crates/v/id_effect_cli.svg)](https://crates.io/crates/id_effect_cli) | CLI helpers for id_effect: optional clap, Exit/Cause → ExitCode, tracing buffer, run_main |
| [`id_effect_kit`](crates/id_effect_kit) | [![crates.io](https://img.shields.io/crates/v/id_effect_kit.svg)](https://crates.io/crates/id_effect_kit) | One front door to id_effect: a single dependency with feature flags per battery and a prelude |
| [`id_effect_opentelemetry`](crates/id_effect_opentelemetry) | [![crates.io](https://img.shields.io/crates/v/id_effect_opentelemetry.svg)](https://crates.io/crates/id_effect_opentelemetry) | OpenTelemetry tracing, metrics and logs integration for id_effect (OTLP export, propagation) |
| [`id_effect_platform`](crates/id_effect_platform) | [![crates.io](https://img.shields.io/crates/v/id_effect_platform.svg)](https://crates.io/crates/id_effect_platform) | Platform capability traits (HTTP, FS, subprocess commands) for id_effect — @effect/platform-style boundaries |
| [`id_effect_sql`](crates/id_effect_sql) | [![crates.io](https://img.shields.io/crates/v/id_effect_sql.svg)](https://crates.io/crates/id_effect_sql) | Driver-agnostic SQL client traits for id_effect — @effect/sql-style boundaries |
| [`id_effect_tool`](crates/id_effect_tool) | [![crates.io](https://img.shields.io/crates/v/id_effect_tool.svg)](https://crates.io/crates/id_effect_tool) | The `id-effect` command: `id-effect new` scaffolds a service, worker, CLI or cluster node on id_effect_kit; `id-effect migrate` runs SQL migrations |

**Platform (batteries)**

| Crate | Version | Description |
|-------|---------|-------------|
| [`id_effect_auth`](crates/id_effect_auth) | [![crates.io](https://img.shields.io/crates/v/id_effect_auth.svg)](https://crates.io/crates/id_effect_auth) | Authentication for id_effect services: password hashing, sessions, JWT, API-key and OIDC credentials |
| [`id_effect_authz`](crates/id_effect_authz) | [![crates.io](https://img.shields.io/crates/v/id_effect_authz.svg)](https://crates.io/crates/id_effect_authz) | Authorization for id_effect services: roles and permissions, named and resource policies, route guards |
| [`id_effect_redis`](crates/id_effect_redis) | [![crates.io](https://img.shields.io/crates/v/id_effect_redis.svg)](https://crates.io/crates/id_effect_redis) | Redis for id_effect services: pooled client layer, typed key-value, hash and JSON stores, a Cache backend, pub/sub streams, sessions and rate limits |
| [`id_effect_test`](crates/id_effect_test) | [![crates.io](https://img.shields.io/crates/v/id_effect_test.svg)](https://crates.io/crates/id_effect_test) | Integration-test support for id_effect: effect-native containers, per-test database isolation, fixtures |

**Maintenance (frozen API)**

| Crate | Version | Description |
|-------|---------|-------------|
| [`id_effect_fp`](crates/id_effect_fp) | [![crates.io](https://img.shields.io/crates/v/id_effect_fp.svg)](https://crates.io/crates/id_effect_fp) | Functional-programming toolkit for id_effect: algebraic type classes, persistent collections, STM (maintenance tier) |
| [`id_effect_optics`](crates/id_effect_optics) | [![crates.io](https://img.shields.io/crates/v/id_effect_optics.svg)](https://crates.io/crates/id_effect_optics) | Functional optics — Lens, Prism, Traversal, JSON patch, and schema field paths for id_effect |

**Labs (unpublished, no guarantees)**

| Crate | Version | Description |
|-------|---------|-------------|
| [`id_effect_ai`](labs/id_effect_ai) | unpublished | Vendor-neutral LLM client traits and streaming for id_effect |
| [`id_effect_events`](labs/id_effect_events) | unpublished | Event sourcing, projections, and CQRS boundaries for id_effect programs |
| [`id_effect_graph`](labs/id_effect_graph) | unpublished | Directed acyclic graphs and topological sorting for id_effect programs |
| [`id_effect_jobs`](labs/id_effect_jobs) | unpublished | Production async messaging — Apalis jobs, obix inbox, Kafka broker |
| [`id_effect_parse`](labs/id_effect_parse) | unpublished | Parser combinators, pretty printing, invertible codecs, and value diffs for id_effect |
| [`id_effect_rpc`](labs/id_effect_rpc) | unpublished | RPC client/server stack and HTTP boundary helpers for id_effect (@effect/rpc parity) |
| [`id_effect_workflow`](labs/id_effect_workflow) | unpublished | Durable step log with SQLite (memory) or duroxide-pg (production) resume semantics |

Tiers and their quality bar: [ADR 0010](docs/adrs/0010-crate-tiers.md), [docs/QUALITY_BAR.md](docs/QUALITY_BAR.md).
<!-- crates:end -->

---

## Minimal example

Add the crate:

```toml
[dependencies]
id_effect = "0.9"
```

```rust
use id_effect::{Effect, Never, succeed};

fn greet(name: &str) -> Effect<String, Never> {
    succeed(format!("Hello, {name}!"))
}

fn main() {
    let exit = greet("world").block_on();
    println!("{exit:?}");
}
```

For a guided path through the API, read [**Typed Effects in Rust**](https://industrial.github.io/id_effect/) first, then use the numbered examples under [`crates/id_effect/examples/`](crates/id_effect/examples/) and [docs.rs](https://docs.rs/id_effect).

**CLI template:** [`examples/cli-minimal/`](examples/cli-minimal/) is the project template: `clap` + [`id_effect_cli`](crates/id_effect_cli) + [`id_effect_config`](crates/id_effect_config) (`Secret`). Run:

```bash
devenv shell -- cargo run -p cli_minimal -- --token dummy
```

---

## Documentation

| Resource | Link |
|----------|------|
| **Book (primary learning path)** | [**Typed Effects in Rust**](https://industrial.github.io/id_effect/) — [glossary](https://industrial.github.io/id_effect/appendix-c-glossary.html) |
| **CLI with `clap` + `ExitCode`** | [CLI with clap (`id_effect_cli`)](https://industrial.github.io/id_effect/part3/ch16-00-cli-with-clap.html) |
| API reference | [docs.rs/id_effect](https://docs.rs/id_effect) |
| Examples | [`crates/id_effect/examples/`](crates/id_effect/examples/) |

---

## Development

This repository uses [devenv](https://devenv.sh). Run commands inside the dev shell:

```bash
devenv shell -- <command>
```

Common [Moon](https://moonrepo.dev) tasks:

```bash
# Format
devenv shell -- moon run :format

# Check + clippy
devenv shell -- moon run :clippy

# Tests (nextest)
devenv shell -- moon run :test

# Coverage (95% threshold)
devenv shell -- moon run :coverage

# Build
devenv shell -- moon run :build

# Run examples for a crate
devenv shell -- moon run id_effect_lib:examples

# Security audit
devenv shell -- moon run :audit

# API docs + mdBook
devenv shell -- moon run :docs :book

# CI parity (same as GitHub moon-ci job)
devenv shell -- bash scripts/ci-local.sh affected

# Pre-push checks (ci-local + coverage + audit)
devenv shell -- pre-push
```

### Continuous integration

| Workflow | Triggers | What it does |
|----------|----------|--------------|
| [CI](https://github.com/Industrial/id_effect/actions/workflows/ci.yml) | push/PR → `main` | Format, clippy, nextest, coverage (≥ 95 %), MSRV (1.94), Postgres integration, labs workspace, minimal versions, packaging, version drift, matrix (stable+beta × linux+mac+win) |
| [Docs & Pages](https://github.com/Industrial/id_effect/actions/workflows/docs.yml) | push/PR → `main` | API docs + mdBook → GitHub Pages |
| [Security Audit](https://github.com/Industrial/id_effect/actions/workflows/audit.yml) | daily + `Cargo.lock` changes | `cargo audit` |
| [Publish](https://github.com/Industrial/id_effect/actions/workflows/publish.yml) | `v*.*.*` tag | Test, then publish crates in dependency order |

### Releases

```bash
# 1. Bump [workspace.package] version in the root Cargo.toml (all crates inherit it;
#    keep labs/Cargo.toml in lockstep) and move CHANGELOG "Unreleased" under the version.
# 2. bash scripts/validate-crate-packaging.sh   # publish order + packaging check
# 3. Commit, push, then tag — CI publishes in dependency order
git tag v0.7.0
git push --tags
```

The publish order has one source: `scripts/validate-crate-packaging.sh --print-order`. Labs crates are never published.

Publishing expects `CARGO_REGISTRY_TOKEN` on crates.io; `CODECOV_TOKEN` is optional for coverage uploads. The publish workflow can use a `crates-io` GitHub environment for approval gates.

---

## Coverage

[![codecov](https://codecov.io/gh/Industrial/id_effect/branch/main/graph/badge.svg)](https://codecov.io/gh/Industrial/id_effect)

CI enforces **≥ 95%** lines, regions, and functions via `cargo llvm-cov nextest`.

![Coverage sunburst](https://codecov.io/gh/Industrial/id_effect/branch/main/graphs/sunburst.svg)

---

## Star history

[![Star History Chart](https://api.star-history.com/svg?repos=Industrial/id_effect&type=Date)](https://star-history.com/#Industrial/id_effect&Date)

---

## Contributors

Thanks to everyone who has contributed patches, reported issues, or improved the types.

[![Contributors](https://contrib.rocks/image?repo=Industrial/id_effect)](https://github.com/Industrial/id_effect/graphs/contributors)

---

## License

This project is licensed under the [MIT License](LICENSE) from 0.8.0 onwards. Versions up to
and including 0.7.0 were published under CC-BY-SA-4.0 and keep that license. Contributions
require a DCO sign-off; see [CONTRIBUTING.md](CONTRIBUTING.md).
