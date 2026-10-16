# Workspace tooling (macros and lints)

This appendix covers **authoring** and **static analysis** pieces that most application readers skip—but contributors and advanced users need to know where they live.

## `id_effect_macro` and `id_effect_proc_macro`

The macro surface is split across two crates, both re-exported at the `id_effect` root (and in
`id_effect::macros`):

- **`id_effect_proc_macro`** — procedural macros: `effect!` / `effect_local!` (do-notation over
  a typed `R`), `caps!`, and the derives (`Schema`, `EffectData`, `ProviderSpec` as
  `ProviderSpecDerive`, `Optics`, `Fsm`, …).
- **`id_effect_macro`** — declarative macros: `pipe!` and `err!`. (The 0.6 capability-DI macros
  `provide!`, `providers!`, `require!` and `mock_capability!` were removed in 0.7.)

Expansions name root paths (`::id_effect::HCons`, `::id_effect::__private::…`), so depend on
`id_effect` rather than on these crates directly. The 0.7 trybuild suite
(`crates/id_effect/tests/ui`) pins the macro diagnostics.

When debugging “why doesn’t my `effect!` compile?”, use **`cargo expand`** on a small repro and inspect the generated code. Application code should keep following [The effect! Macro](./part1/ch03-00-effect-macro.md); these crates are implementation details unless you extend the macro system.

## `id_effect_lint`

> **Labs crate — unpublished, no API guarantees ([ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md)).**
> Use it via a path or git dependency on `labs/`.

Custom **Rustc lint** crate for id_effect-specific rules lives at **`labs/id_effect_lint`**, in the separate labs workspace, so normal **`cargo check --workspace`** never builds it. Work on it with `cargo check --manifest-path labs/Cargo.toml -p id_effect_lint`.

For day-to-day coding, rely on **`clippy`** plus repository **[`TESTING.md`](https://github.com/Industrial/id_effect/blob/main/TESTING.md)**; treat **`id_effect_lint`** as an additional enforcement layer when integrated into your compiler invocation.

## Platform scorecard

Each release is scored against ten reference frameworks on 82 capabilities. The rules are in
[`docs/scorecard/METHOD.md`](https://github.com/Industrial/id_effect/blob/main/docs/scorecard/METHOD.md),
and the rendered matrix, with a version selector and the planned path to 1.0, is
[`docs/scorecard/index.html`](https://github.com/Industrial/id_effect/blob/main/docs/scorecard/index.html).
`scripts/scorecard.sh` computes every number from `docs/scorecard/scorecard.json`, writes the
per-release report (for example `docs/scorecard/REPORT-0.7.0.md`) and runs the release gates
`--diff` and `--compare-targets`.
