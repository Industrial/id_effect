# DX and deploy

Platform DX ships deploy templates and CLI parity documented under `docs/platform/`.

## Starting an app

There is no `id-effect new` generator (the earlier placeholder scaffolder was removed in 0.6). Start from the [`examples/cli-minimal`](https://github.com/Industrial/id_effect/tree/main/examples/cli-minimal) template: `app_effect` returns an `Effect<(), ConfigError, ConfigEnv>` and `main` calls `id_effect_cli::run_main(eff, env, RunMainConfig::with_tracing())`. For an HTTP service, start from [`examples/http-showcase`](https://github.com/Industrial/id_effect/tree/main/examples/http-showcase). See [PHASE_E_CLI_PARITY.md](https://github.com/Industrial/id_effect/blob/main/docs/platform/PHASE_E_CLI_PARITY.md).

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
