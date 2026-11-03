---
title: 'Warden — operations and the §6 exit KPI: console profiling, chaos in CI, prod posture, README, reference-warden in ci-pass: r14-w-warden-ops'
slug: r14-w-warden-ops
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'D1 item 6 and the PLAN-OF-PLANS §6 row 0.14.0 exit KPI ("Warden reference app green; every promoted row cites a reference-app CI test") in examples/reference/warden, through id_effect_kit only (adds feature `console`): `/_console` (id_effect_console, API under `/_console/api`, crates/id_effect_console/src/api.rs:26) mounted behind the Warden''s auth guard, with the on-demand CPU/heap profiling or stack-dump endpoints of r14-s-console-profiling (obs.console#7) exercised against live agent fibers; runtime fault toggles of r14-s-chaos (test.chaos#5) wired to the model, tool and broker calls; a toxiproxy (r14-s-chaos, test.chaos#6) between the three nodes and between nodes and the brokers in the chaos tests; the prod posture (crates/id_effect_kit/src/posture.rs, `Posture` :331, `AcceptedRisks` :256) clean with zero accepted risks. Rows: obs.console 75 → 88 and test.chaos 75 → 100 are met by the sweep leaves; this leaf gives both rows a reference-app CI test (they are already production). It promotes no row itself; it closes the KPI for the six Warden rows (fault.fsm, conc.stm from r14-w-warden-core; fault.supervision, dist.membership, conc.coordination, core.race from r14-w-warden-mesh).'
  - 'Tests: tests/console.rs `warden_console_requires_auth`, `warden_console_fiber_dump_shows_agent_tree`, `warden_console_cpu_profile_on_demand`, `warden_console_heap_profile_or_stack_dump_on_demand`, `warden_console_profiling_closed_in_prod`; tests/chaos.rs (`ID_EFFECT_REQUIRE_DOCKER=1`) `warden_fault_toggle_fails_tool_calls_at_runtime`, `warden_fault_toggle_cleared_runs_recover`, `warden_toxiproxy_latency_between_nodes_runs_complete`, `warden_toxiproxy_partition_replaces_agents`, `warden_toxiproxy_broker_reset_no_duplicate_effects`; tests/posture.rs `warden_prod_posture_is_clean` (zero accepted risks, every route public or guarded, no `dangerous_*` recorded), `warden_prod_refuses_to_start_with_fault_toggles_enabled`, `warden_prod_refuses_to_start_on_a_failed_probe` (pattern: examples/reference/relay/tests/posture.rs:45, :110); tests/manifest.rs `warden_manifest_depends_on_kit_only` (cargo metadata, pattern examples/reference/relay/tests/manifest.rs); tests/evidence.rs `warden_scorecard_evidence_names_existing_tests` (parses the README "Scorecard evidence" table and fails if a cited test function is missing from tests/*.rs or a promoted Warden row has no row in the table).'
  - 'Book and example: no new chapter (obs.console in part6/ch27-03-console.md, test.chaos in part4/ch15-09-fault-injection.md, which r14-s-console-profiling and r14-s-chaos extend); README final: sections "Operations" (console, profiling), "Chaos", "Secure by default", "Run it" (compose.yaml with Postgres, Redis, NATS, toxiproxy), and the complete "Scorecard evidence" table (row → check → test → file → CI job `reference-warden`) that r14-x-level-audit cites; the README `rust` fence still compiles as a doctest.'
  - 'CI job `reference-warden` final in .github/workflows/ci.yml: Postgres, Redis, NATS (JetStream) and toxiproxy images pulled up front; `ID_EFFECT_REQUIRE_PG: 1` and `ID_EFFECT_REQUIRE_DOCKER: 1`; fmt, clippy `-D warnings`, `cargo nextest run --locked -p warden --all-features` (chaos tests included, none `#[ignore]`d), the posture test, `bash scripts/tier-conformance.sh`; the job header comment states the KPI; `reference-warden` added to the `ci-pass` `needs` list (lines 962–979). Green twice on the release branch before the level audit.'
  - 'Tier: example (publish = false). Secure by default: ADR 0016 Security rows applied in the app — R7 `/_console` and the profiling endpoints closed in prod unless guarded, and guarded here (compile-fail `r7_profiling_endpoint_without_guard` is r14-e-fuzz-security''s); R10 the prod profile refuses to start with any fault toggle enabled or any toxiproxy/`dangerous_*` opt-out recorded; R4 profiling duration and output caps; R1 every route declared; zero entries in `AcceptedRisks`.'
  - 'Coverage: tier example is outside the 96 % region gate; report `ID_EFFECT_REQUIRE_PG=1 ID_EFFECT_REQUIRE_DOCKER=1 cargo llvm-cov nextest -p warden --all-features --summary-only` in the leaf evidence.'
  - 'Gate: `ID_EFFECT_REQUIRE_PG=1 ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p warden --all-features`; `cargo test -p warden --doc --all-features`; `cargo clippy -p warden --all-targets --all-features -- -D warnings`; `cargo fmt -p warden --check`; `bash scripts/tier-conformance.sh`; `bash scripts/check-workflows.sh` (pinned actions); CI job `reference-warden` green and required by `ci-pass`'
  - 'Owns: `examples/reference/warden/src/{console.rs,chaos.rs,posture.rs}`, `examples/reference/warden/tests/{console.rs,chaos.rs,posture.rs,manifest.rs,evidence.rs}`, `examples/reference/warden/tests/common/toxiproxy.rs`, `examples/reference/warden/README.md` (final; all sections), `examples/reference/warden/compose.yaml` (toxiproxy service, append-only), `examples/reference/warden/Cargo.toml` (kit feature list only), `examples/reference/warden/src/lib.rs` and `src/main.rs` (module lines and the console/posture wiring at startup), `examples/reference/warden/tests/common/mod.rs` (append-only `mod toxiproxy;`), `crates/id_effect_kit/Cargo.toml` (only: a non-dev `fault-toggles` feature forwarding `id_effect_test/fault-toggles` so the running app, not just its tests, can hold `FaultToggles` — r14-s-chaos already appends both features to the `test` list — and the console profiling forward if r14-s-console-profiling put it behind an id_effect_console feature), `crates/id_effect_kit/src/lib.rs` (feature-table rows and one `pub use`, append-only), `.github/workflows/ci.yml` (job `reference-warden`, and the single `- reference-warden` line in `ci-pass` needs), `CHANGELOG.md` (append-only), `Cargo.lock` (append-only)'
---

# r14-w-warden-ops

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.2 — Promote (19 rows) and Warden" (the §6
exit KPI). The release spec's row is binding; this file exists so Maestro can track the leaf as its
own task.

Wave 5, critical path. Blocked by: r14-w-warden-mesh (W4), r14-s-console-profiling (W3; the
profiling endpoints), r14-s-chaos (W3; runtime fault toggles and the toxiproxy container in
id_effect_test). Blocks r14-x-level-audit (W6), which cites the README "Scorecard evidence" table,
and r14-x-rescore. Never cut. Runs beside r14-p-toolchain and r14-e-benches: if either edits
`ci-pass`, it rebases after this leaf (this leaf adds one `needs` line only). Toolchain commands run
as `devenv shell -- <cmd>`.

Defaults taken (owner may overturn): the CI job runs four containers (Postgres, Redis, NATS and
toxiproxy; the prompt named three, but test.chaos#6 in a reference app needs the proxy); chaos tests
run in the same job, not a separate one; the README evidence table is checked by a test so the level
audit cannot cite a test that does not exist; r14-s-chaos ships the toggles and the toxiproxy client as
id_effect_test features `fault-toggles` and `toxiproxy` and adds them to the kit `test` list only
(a dev-dependency feature), so this leaf adds the non-dev kit forward the running app needs (the Warden is kit-only, D1; kit coverage gate `cargo llvm-cov nextest -p id_effect_kit
--all-features --summary-only --fail-under-regions 96` then applies); `src/main.rs` is shared with r14-w-warden-core only
across waves (W3 → W5), never in parallel.
