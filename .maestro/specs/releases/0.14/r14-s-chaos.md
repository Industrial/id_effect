---
title: 'E13.1 Sweep — runtime fault toggles (off in prod) and toxiproxy network faults: r14-s-chaos'
slug: r14-s-chaos
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'test.chaos#5 (row test.chaos 75 → 88 with #6 → 100, level production unchanged): runtime fault-injection toggles in a running app. crates/id_effect_test, feature `fault-toggles` (on top of `fault`): `FaultToggles` is a capability holding named fault points, each an existing `Fault` (crates/id_effect_test/src/fault/mod.rs:83 delay, :88 fail, :95 timeout) or a seeded `FaultPlan` (:144, :158) with a probability; `toggles.inject("payments.charge", call)` wraps an effect like `FaultPlan::inject` (:248) and is a pass-through for a point that is off. Toggles change while the app runs: `FaultToggles::set` / `clear` (programmatic), and `FaultToggles::follow(subscription)` re-applies a `[faults]` config table (`FaultsConfig`, serde) every time an `id_effect_config` `ProfileWatch` publishes a new value (crates/id_effect_config/src/watch.rs:190 `ProfileWatch`, :244 `subscription`), so editing `config/dev.toml` toggles a fault without a restart. Every change is logged with the point name and the fault kind, and counted in `toggles.history()`.'
  - 'test.chaos#6: network-proxy fault injection integrated. crates/id_effect_test, feature `toxiproxy`: a `Toxiproxy` container (Shopify toxiproxy image pinned by tag and sha256 digest, as `MAILPIT_TAG` is, crates/id_effect_test/src/containers/mailpit.rs:24) started as a scoped layer like the other containers; `ToxiproxyContainer::proxy(name, upstream)` creates a proxy to another container (reached by its Docker network address, a new `Container::network_address(port)` beside `address` container.rs:91) or a host address, and returns the host address clients use; `Proxy::add(Toxic::latency(..) | Toxic::bandwidth(..) | Toxic::timeout(..) | Toxic::reset_peer() | Toxic::slow_close(..))`, `Proxy::disable()` / `enable()` (connection down), `Proxy::remove_toxic(name)`; toxiproxy''s HTTP API is spoken over the crate''s own tokio runtime (no new HTTP client; `serde_json`, already optional, for the bodies). Readiness bounded by `ready_timeout` (typed `ContainerError::NotReady`).'
  - 'Secure by default (ADR 0013 R10, Security table row "Fault toggles"): with profile `prod` (`FaultToggles::for_profile`, default `ID_EFFECT_PROFILE`) every toggle is inert, and a `[faults]` table that enables any point records `id_effect_test::fault::FaultToggles::dangerous_enabled_in_prod` through `id_effect::observability::posture::record_dangerous` (crates/id_effect/src/observability/posture.rs:34), so the kit''s prod posture check (crates/id_effect_kit/src/posture.rs:1254 `check`) refuses to start unless the operator accepted that id; listed by `scripts/check-dangerous.sh`. The toggles are never exposed over HTTP by this leaf (no route, no console panel).'
  - 'Tests: crates/id_effect_test/tests/fault_toggles.rs `toggle_off_is_pass_through`, `set_toggle_faults_running_effect`, `config_reload_toggles_fault_without_restart` (ProfileWatch on a temp dir under the TestExecutor clock), `seeded_toggle_replays_same_schedule`, `prod_profile_keeps_toggles_inert`, `prod_profile_enabled_toggle_records_dangerous`; crates/id_effect_kit/tests/posture_chaos.rs `prod_posture_refuses_enabled_fault_toggles`, `prod_posture_accepts_toggles_when_operator_accepted`; crates/id_effect_test/tests/toxiproxy.rs (Docker; `ID_EFFECT_REQUIRE_DOCKER=1` fails rather than skips) `toxiproxy_latency_delays_postgres_queries`, `toxiproxy_disable_fails_redis_then_recovers`, `toxiproxy_reset_peer_surfaces_as_typed_error`, `toxiproxy_container_removed_when_scope_closes`; plus the unit tests of the API request bodies in `src/containers/toxiproxy.rs`.'
  - 'Book: crates/id_effect/book/src/part4/ch15-09-fault-injection.md new sections "Runtime toggles" and "Network faults through a proxy" (existing sections unchanged). Examples crates/id_effect_test/examples/060_fault_toggles.rs (feature `fault-toggles`) and 070_toxiproxy.rs (feature `toxiproxy`, prints SKIP without Docker unless `ID_EFFECT_REQUIRE_DOCKER=1`). ch15-09 is already compiled by examples/book-snippets/src/part4.rs:84.'
  - 'Tier: platform (id_effect_test, unchanged) and platform (id_effect_kit: only the `test` feature list grows). No new dependency.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_test --all-features --summary-only --fail-under-regions 96`'
  - 'Gate: `cargo nextest run -p id_effect_test --features fault-toggles,toxiproxy`; `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_test --all-features --test toxiproxy` (the existing containers job, .github/workflows/ci.yml:347, already runs `-p id_effect_test --all-features` with `ID_EFFECT_REQUIRE_DOCKER=1`, so no ci.yml edit); `cargo nextest run -p id_effect_kit --features test --test posture_chaos`; `cargo clippy -p id_effect_test -p id_effect_kit --all-targets --all-features -- -D warnings`; `bash scripts/check-dangerous.sh`; `bash scripts/tier-conformance.sh id_effect_test`'
  - 'Owns: `crates/id_effect_test/src/fault/toggles.rs` (new), `crates/id_effect_test/src/fault/mod.rs` (the `toggles` module line and its re-exports; append-only), `crates/id_effect_test/src/containers/toxiproxy.rs` (new), `crates/id_effect_test/src/containers/mod.rs` (module and `pub use` lines; append-only), `crates/id_effect_test/src/containers/container.rs` (new fn `Container::network_address` only), `crates/id_effect_test/src/lib.rs` (crate-doc bullets for the two features; append-only), `crates/id_effect_test/Cargo.toml` (features `fault-toggles` (with `dep:serde`), `toxiproxy` (with `dep:serde_json`), an `id_effect_config` dev-dependency for the reload test, and the two `[[example]]` entries), `crates/id_effect_test/tests/{fault_toggles,toxiproxy}.rs`, `crates/id_effect_test/examples/{060_fault_toggles,070_toxiproxy}.rs`, `crates/id_effect_test/README.md` (feature list), `crates/id_effect_kit/tests/posture_chaos.rs` (new), `crates/id_effect_kit/Cargo.toml` (append-only: the two tokens `id_effect_test/fault-toggles` and `id_effect_test/toxiproxy` in the `test` feature list, nothing else), `crates/id_effect/book/src/part4/ch15-09-fault-injection.md` (the two new sections), append-only: `CHANGELOG.md`, `Cargo.lock`'
---

# r14-s-chaos

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r14-s-testing (wave 1; same `id_effect_test` lib.rs / Cargo.toml / README). Blocks
r14-w-warden-ops (wave 5: Warden toggles faults through its config in dev and runs one network-fault
test through `Toxiproxy`), which is where `test.chaos` keeps its production level with the two new
checks cited. Cut order 5 in the release spec (#6 only) is not available: Warden needs it.

Shared file in this wave: r14-s-console-profiling also edits crates/id_effect_kit/Cargo.toml. This
leaf's edit is two tokens appended to the `test` feature list (r14-w-warden-ops relies on them); the
coordinator merges the two leaves in sequence, or moves the two tokens into r14-w-warden-ops if it wants
the file strictly single-owner in wave 3 (the kit posture test then moves with them).

Defaults the owner may overturn: (1) "via config or the console": this leaf does config hot reload plus
a programmatic handle and no console panel, because `crates/id_effect_console/src/api.rs` (`Console`
builder, :45) belongs to r14-s-console-profiling in the same wave; a `Console::faults(..)` panel is a
small follow-up for wave 4+ (r14-w-warden-ops can own it, or the console leaf adds the builder method
if the coordinator prefers). (2) Toggles live in id_effect_test (the prompt's home for chaos); an app
uses them behind its own `chaos` cargo feature, so a prod build need not link them, and the profile
rule above covers a build that does. (3) Toxiproxy reaches an upstream container by its bridge-network
address (Docker inspect), not a per-test Docker network; rootless Docker is covered by the same
inspect call, re-check at EXECUTE.
