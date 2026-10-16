# Phase E CLI parity — audit (wave 0)

Reference: [phase-e-cli.md](../effect-ts-parity/phases/phase-e-cli.md)

> **Historical audit (pre-0.7).** API names below are as of the audit. In 0.7 `run_main` no longer
> wraps `run_blocking` (removed with the v1 interpreter): it runs the effect on the fiber runtime's
> blocking edge and maps the `Exit` to an `ExitCode`. See `crates/id_effect_cli/src/run.rs`. The
> two wave 0 gaps below were closed in 0.8 by `id_effect_tool` (`id-effect new`, `id-effect doctor`).

## Shipped in `id_effect_cli`

| Phase E slug | Requirement | Status |
|--------------|-------------|--------|
| `iep-e-011` | Exit / Cause → `ExitCode` mapping | **Shipped** — `exit_code.rs`, mdBook ch16-01 |
| `iep-e-020` | `id_effect_cli` skeleton + `clap` feature | **Shipped** |
| `iep-e-021` | `run_main` helper (tracing + `run_blocking`; 0.7: the blocking edge) | **Shipped** — `run.rs` |
| `iep-e-022` | Integration test exit codes 0/1 | **Shipped** — `tests/cli_probe_exit.rs`, `ie_cli_probe` |
| `iep-e-030` | `examples/cli-minimal` template | **Shipped** |
| `iep-e-010` | mdBook CLI entrypoints chapter | **Shipped** — part3 ch16-* |

## Wave 0 closure (this task)

| Gap | Resolution |
|-----|------------|
| No unified developer CLI entrypoint | **Closed in 0.8** — `id-effect` (crate `id_effect_tool`, `cargo install id_effect_tool`) with `new` and `doctor`. The 0.6 placeholder binary is gone, and `id-effect-diagnose` was removed in 0.7 |
| No app generator | **Closed in 0.8** — `id-effect new <name> --template service\|worker\|cli\|cluster` (the `cli` template is the `examples/cli-minimal` shape); `id-effect doctor` checks a project's toolchain, versions, environment, database, docker and port |

## Remaining (future waves)

| Item | Notes |
|------|-------|
| stdout/stderr `Sink` adapters | Documented as future in crate docs; defer to platform-ui or logger integration |
| `iep-e-012` Config + Secret flags snippet | Partially covered by cli-minimal + ch16-02; no standalone snippet crate |
| `@effect/cli` parser-combinator parity | **Non-goal** per Phase E — embrace `clap` |

## Verify

```bash
cargo run -p id_effect_tool --bin id-effect -- --help
cargo nextest run -p id_effect_cli -p id_effect_tool
```
