# DX and deploy

Platform DX ships generators, deploy templates, and CLI parity documented under `docs/platform/`.

## CLI and app generator

```bash
id-effect new my-app --template minimal
```

See [`id_effect_cli::generator`](../../../id_effect_cli/src/generator.rs) and [PHASE_E_CLI_PARITY.md](../../../../docs/platform/PHASE_E_CLI_PARITY.md).

## Kamal deploy template

Scaffold under `crates/id_effect_cli/templates/deploy-kamal/`:

- `config/deploy.yml.template` — Kamal 2 service definition
- `Dockerfile.template` — multi-stage Rust release build

Copy to your app root and replace `{{name}}` placeholders (same tokens as `id-effect new`).

## Admin scaffold stub

`templates/admin-stub/` provides a README and placeholder Axum routes for an internal admin UI — not a full Django-admin clone.

## See also

- Mission: `platform-dx-ship`
- [ROADMAP.md](../../../../docs/platform/ROADMAP.md)
