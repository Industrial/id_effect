# Phase E CLI parity — audit (wave 0)

Reference: [phase-e-cli.md](../effect-ts-parity/phases/phase-e-cli.md)

## Shipped in `id_effect_cli`

| Phase E slug | Requirement | Status |
|--------------|-------------|--------|
| `iep-e-011` | Exit / Cause → `ExitCode` mapping | **Shipped** — `exit_code.rs`, mdBook ch16-01 |
| `iep-e-020` | `id_effect_cli` skeleton + `clap` feature | **Shipped** |
| `iep-e-021` | `run_main` helper (tracing + `run_blocking`) | **Shipped** — `run.rs` |
| `iep-e-022` | Integration test exit codes 0/1 | **Shipped** — `tests/cli_probe_exit.rs`, `ie_cli_probe` |
| `iep-e-030` | `examples/cli-minimal` template | **Shipped** |
| `iep-e-010` | mdBook CLI entrypoints chapter | **Shipped** — part3 ch16-* |

## Wave 0 closure (this task)

| Gap | Resolution |
|-----|------------|
| No unified developer CLI entrypoint | **Closed** — `id-effect` binary with `version` subcommand |
| No app generator | **Closed (stub)** — `id-effect new <name>` scaffolds `templates/app-minimal/` |

## Remaining (future waves)

| Item | Notes |
|------|-------|
| stdout/stderr `Sink` adapters | Documented as future in crate docs; defer to platform-ui or logger integration |
| `iep-e-012` Config + Secret flags snippet | Partially covered by cli-minimal + ch16-02; no standalone snippet crate |
| `@effect/cli` parser-combinator parity | **Non-goal** per Phase E — embrace `clap` |

## Verify

```bash
cargo run -p id_effect_cli --features clap --bin id-effect -- version
cargo run -p id_effect_cli --features clap --bin id-effect -- new demo --dest /tmp/demo-check
```
