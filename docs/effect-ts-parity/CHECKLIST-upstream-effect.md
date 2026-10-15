# Upstream Effect.ts parity checklist

**Purpose:** Run this checklist **each release cycle** (see Phase I, `iep-i-010`–`iep-i-011`). Record the **upstream Effect version** reviewed in [`UPSTREAM-VERSION`](./UPSTREAM-VERSION).

**How to use:** For each row, mark **Reviewed**, **Gap noted** (with link to Beads issue), or **N/A** for Rust. Keep links to the canonical Effect docs or source.

| Upstream area | Effect.ts / docs anchor | id_effect location / notes | Status |
|---------------|-------------------------|----------------------------|--------|
| Core `Effect` | `effect` package `Effect` | `crates/id_effect/src/kernel/` (`Effect<A, E = Never, R = HNil>`, `Send`, one-shot; `LocalEffect` for `!Send`), `effect!` / `effect_local!` | |
| `Context` / `Layer` | Services, layers | `crates/id_effect/src/capability/` (typed `R` HList, `need`, `provide*`, `narrow`; a missing service is a compile error), `crates/id_effect/src/layer.rs` (memoised per build, scoped finalizers) | |
| `Scope` / `acquireRelease` | Resource safety | `crates/id_effect/src/scope/`, `kernel/resource.rs` (`acquire_use_release`, `ensuring`, masks), `Layer::scoped` / `Layer::acquire_release` | |
| `Schedule` / retry | Scheduling policies | `crates/id_effect/src/scheduling/schedule.rs` (immutable `Schedule`; `Effect::retry` / `repeat` take an effect factory) | |
| `Stream` / `Sink` / `Channel` | Streaming | `crates/id_effect/src/streaming/` (pull, chunked) | |
| `STM` | `STM` module | `crates/id_effect_fp/src/stm/` (maintenance tier) | |
| `Schema` | Parse/encode | `crates/id_effect/src/schema/` | |
| `Fiber` / interruption | Concurrency | `crates/id_effect/src/runtime/fiber.rs` (`FiberHandle`), `concurrency/` (`fork_all`), `par.rs` (`all`, `zip_par`), `race.rs` (`race`, `timeout`) | |
| `FiberRef` | Fiber-local state | `crates/id_effect/src/runtime/fiber_ref.rs` | |
| Queue / PubSub / Deferred / Semaphore / Ref | Coordination | `crates/id_effect/src/coordination/` | |
| `Pool` / `Cache` | Resources | `crates/id_effect/src/resource/` | |
| `Request` / `RequestResolver` | Batching, caching | `crates/id_effect/src/request.rs` | |
| `Metric` / `Tracer` | Observability | `crates/id_effect/src/observability/` (`MetricRegistry`, `Effect::with_span`, `SpanHook`) | |
| `TestClock` / `TestServices` | Testing | `crates/id_effect/src/testing/` (`TestExecutor` virtual time, `TestRandom`) | |
| `Cause` / `Exit` | Failure | `crates/id_effect/src/failure/` | |
| `@effect/platform` | HTTP, FS, process | **Phase A** — `crates/id_effect_platform` (`http`, `fs`, `command`, `uri`, opt-in `auth`) | |
| `@effect/opentelemetry` | OTEL | **Phase B** — `crates/id_effect_opentelemetry` (`OtelSpanHook`, `bridge_metrics`) + mdBook `part2/ch07-12-opentelemetry.md` | |
| `@effect/sql` | Database | **Phase C** — `crates/id_effect_sql` (`SqlClient`, `with_transaction`; Postgres in `id_effect_sql::pg`, feature `pg`) | |
| `@effect/rpc` | RPC | **Phase D** — `labs/id_effect_rpc` (unpublished) | |
| `@effect/cli` | CLI | **Phase E** — `crates/id_effect_cli` (`run_main`, exit-code mapping); template `examples/cli-minimal` | |
| Supervision | Fiber supervision | `crates/id_effect/src/coordination/supervisor.rs` (`Supervisor`, `SupervisorPolicy`, `supervised`), `crates/id_effect_process` (OTP trees), mdBook §9.5 | Reviewed |
| Cluster / workflow | Distributed | **Phase G** — `crates/id_effect_node` (Process Mesh), `labs/id_effect_workflow` (unpublished) | |
| `@effect/ai` | LLM clients | Partial — `labs/id_effect_ai` (unpublished): traits + OpenAI/Anthropic/Cursor HTTP | tool-calling backlog |

## Review log

| Date | Upstream version | Reviewer | Notes / link |
|------|------------------|----------|--------------|
| 2026-06-19 | effect@3.14.0 | platform-parity-hygiene | UPSTREAM-VERSION bump |
| 2026-10-15 | effect@3.14.0 | r-docs-sweep | Locations updated to the 0.7 kernel (typed `R`, `Layer`, `TestExecutor`); no upstream re-review |
