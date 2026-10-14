# DX and deploy

Platform DX ships deploy templates and CLI parity documented under `docs/platform/`.

## Starting an app

There is no `id-effect new` generator in 0.6 (the earlier placeholder scaffolder was removed). Start from the [`examples/cli-minimal`](https://github.com/Industrial/id_effect/tree/main/examples/cli-minimal) template, which uses `id_effect_cli::run_main`. See [PHASE_E_CLI_PARITY.md](https://github.com/Industrial/id_effect/blob/main/docs/platform/PHASE_E_CLI_PARITY.md).

## Kamal deploy template

Scaffold under `crates/id_effect_cli/templates/deploy-kamal/`:

- `config/deploy.yml.template` — Kamal 2 service definition
- `Dockerfile.template` — multi-stage Rust release build

Copy to your app root and replace the `{{name}}` placeholders with your binary name.

## Admin scaffold stub

`templates/admin-stub/` provides a README and placeholder Axum routes for an internal admin UI — not a full Django-admin clone.

## See also

- Mission: `platform-dx-ship`
- [ROADMAP.md](https://github.com/Industrial/id_effect/blob/main/docs/platform/ROADMAP.md)
