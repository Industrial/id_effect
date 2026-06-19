# `id_effect_host`

Application **host shell** for [`id_effect`](../id_effect) platform services — lifecycle,
config bootstrap, and graceful shutdown hooks for Axum/Tokio servers.

## Modules

| Module | Description |
|--------|-------------|
| [`bootstrap`](src/bootstrap.rs) | Load [`HostConfig`] via `id_effect_config`, build [`Env`](id_effect::Env) |
| [`lifecycle`](src/lifecycle.rs) | [`Host`] builder and run-until-shutdown entry point |
| [`shutdown`](src/shutdown.rs) | Signal handling (Ctrl+C / SIGTERM) and drain |
| [`modules`](src/modules.rs) | Placeholder module graph for auth/security layers |

## Testing

```bash
cargo nextest run -p id_effect_host
```
