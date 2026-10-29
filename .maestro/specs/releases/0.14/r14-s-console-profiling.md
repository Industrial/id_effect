---
title: 'E13.1 Sweep — on-demand CPU profiles behind the console guard: r14-s-console-profiling'
slug: r14-s-console-profiling
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'obs.console#7: what exists is the fiber dump (`GET /_console/api/fibers`, crates/id_effect_console/src/api.rs:143, `FiberView` :170 with each fiber''s span stack) and the supervisor tree (:144); what is missing is a process profile. New in crates/id_effect_console/src/profile.rs (feature `profiling`, new): `POST /_console/api/profile/cpu?seconds=N&hz=H` samples the process with the `pprof` crate (exact pin recorded; features `prost-codec` only, not `flamegraph`, whose `inferno` dependency is CDDL) for N seconds (1 to 30, default 10; hz 1 to 1 000, default 99) and answers the gzipped pprof protobuf (`application/octet-stream`, `Content-Disposition: attachment; filename=cpu.pb.gz`), readable by `go tool pprof`; one profile at a time per process (a second request while one runs is `409` Problem Details); the route exists only when the app calls `Console::profiling(ProfilingConfig)` (otherwise `404`, as `jobs` without a queue, api.rs:130), and it is mounted inside `Console::router(guard)` (api.rs:142), so it is never reachable without the console guard. Beside it, `GET /_console/api/stacks` (same feature and opt-in) answers a jstack-style `text/plain` stack dump built from the core fiber dump: every live fiber with its state, age, parent chain and span stack, capped at 10 000 fibers with a truncation line. The dashboard (src/ui/index.html) gets "CPU profile" and "Stack dump" buttons that download them. Row obs.console 75 → 88 (#4 remote shell is 0.15).'
  - 'Platforms: `pprof` samples with SIGPROF and builds on unix only: the dependency is `[target.''cfg(unix)''.dependencies]`, and on other targets the route answers `501` Problem Details, so the `matrix` job (ci.yml:169-212, windows included) still builds `--all-features`.'
  - 'Tests: crates/id_effect_console/tests/profile.rs `cpu_profile_returns_a_pprof_for_the_window` (decodes the protobuf, has samples from a busy fiber), `cpu_profile_window_and_rate_are_capped` (`seconds=31` and `hz=0` are `400`), `concurrent_cpu_profile_is_a_conflict`, `cpu_profile_is_absent_unless_enabled`, `stack_dump_lists_every_fiber_with_its_span_stack`, `stack_dump_is_capped`, `cpu_profile_is_guarded` (no principal → `401`/`403` over a real socket); both routes join `routes()` in crates/id_effect_console/tests/console.rs:139 so `every_console_route_is_guarded_over_a_socket` (:165) covers it; the profiling-enabled state shows in the posture report (`console_profiling_reported_in_posture`).'
  - 'Book: new section "## Profiling" in `crates/id_effect/book/src/part6/ch27-03-console.md` (after "## The jobs panel", :71) and the new row in its API table (:57); example `crates/id_effect_console/examples/030_profiling.rs` (required-features `profiling`).'
  - 'Tier: platform (id_effect_console; Ledger consumer). Secure by default (ADR 0013): R7 operator surface: off unless enabled in code, only inside the guarded console router (the compile-fail case is r14-e-fuzz-security''s: `r7_profiling_endpoint_without_guard` there, `r2_…` in the release spec''s Security table, whose R-numbers predate ADR 0013''s; there is no public constructor of the route outside `Console::router(guard)`, which that case relies on); R4 bounded: window, rate and one-at-a-time caps above, profile size bounded by the window; `POST` with no body and `Cache-Control: no-store` (the console''s `no_store` layer, api.rs:156), and the console''s CSRF header check applies as for `PUT /log-level`; the enabled state is reported to the posture registry so a prod profile shows it.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_console --all-features --summary-only --fail-under-regions 96` (on Linux).'
  - 'Gate: `cargo nextest run -p id_effect_console --all-features`; `cargo clippy -p id_effect_console --all-targets --all-features -- -D warnings`; `cargo deny check`; `bash scripts/tier-conformance.sh id_effect_console`'
  - 'Owns: `crates/id_effect_console/src/profile.rs` (new), `crates/id_effect_console/src/api.rs` (`Console::profiling`, the `/profile/cpu` and `/stacks` route lines in `router`, the state field), `crates/id_effect_console/src/ui/index.html` (the two buttons only), `crates/id_effect_console/tests/profile.rs` (new), `crates/id_effect_console/tests/console.rs` (`routes()` only), `crates/id_effect_console/examples/030_profiling.rs` (new), `crates/id_effect_console/Cargo.toml` (feature `profiling`, the unix-only `pprof` and `flate2` dependencies, `[[example]]` entry), `crates/id_effect/book/src/part6/ch27-03-console.md` (the section and the table row), the kit feature line `console-profiling` in `crates/id_effect_kit/Cargo.toml`; append-only: `crates/id_effect_console/src/lib.rs` (mod and `pub use` lines), `crates/id_effect_kit/src/lib.rs` (the `console-profiling` feature-table row), `CHANGELOG.md`, `Cargo.lock`'
---

# r14-s-console-profiling

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: the wave-2 merge (no wave-2 leaf owns console files). Blocks r14-w-warden-ops
(wave 5: Warden's `/_console` shows profiling behind auth) and r14-e-fuzz-security (wave 4:
`r7_profiling_endpoint_without_guard`). r14-w-warden-ops names `warden_console_cpu_profile_on_demand` and
`warden_console_heap_profile_or_stack_dump_on_demand`: the second is met by `GET /stacks` below, not by a
heap profile.

Defaults the owner may overturn: a CPU profile plus a fiber stack dump, no heap profile (the check says "CPU/heap profiling or thread/stack
dumps"; heap profiling needs a jemalloc global allocator, which a library must not impose); pprof
output, not a flamegraph SVG (no CDDL dependency; `go tool pprof` or speedscope renders it); the JSON
fiber dump of #1 already carries span stacks, but the scorer counted it for #1 only, so the text stack
dump is the explicit #7 evidence beside the CPU profile. Not done here: heap profiles, OS thread dumps, tokio task dumps (need `tokio_unstable`),
continuous profiling export.
