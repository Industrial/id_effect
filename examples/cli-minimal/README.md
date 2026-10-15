# `cli-minimal` — the id_effect CLI template

This package is **the** starter template for a new command-line program built on id_effect (it replaces
the old `templates/app-minimal`). It mirrors the mdBook **CLI with clap** chapter:

- Parse flags with [`clap`](https://docs.rs/clap).
- Load [`Secret`](https://docs.rs/id_effect_config/latest/id_effect_config/struct.Secret.html) configuration via [`id_effect_config`](https://docs.rs/id_effect_config).
- Finish with [`id_effect_cli::run_main`](https://docs.rs/id_effect_cli) (success → exit `0`, typed error → `1`,
  defect → `101`). `app_effect` returns the program (an `id_effect::Effect` over
  `ConfigEnv`) and its environment; `main` hands both to `run_main`.

## Start a new CLI from it

1. Copy `examples/cli-minimal/` to your new repository and rename the package.
2. Replace the `path = "../../crates/…"` parts of the `id_effect*` dependencies with plain versions,
   keeping the `version = "…"` that is already there — it always equals the current workspace
   release (`scripts/check-version-drift.sh` enforces this in CI).
3. Drop the `.workspace = true` package keys for concrete values (`edition = "2024"`, …).

## Build / run (from repo root)

```bash
cargo build -p cli_minimal
cargo run -p cli_minimal -- --token demo
```

Or with Moon:

```bash
moon run cli_minimal:build
```

## Tests

```bash
cargo nextest run -p cli_minimal
```
