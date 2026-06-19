---
name: id_effect-integration
description: >-
  Teaches id_effect workspace integration: id_effect_tokio run_async, id_effect_platform
  I/O, id_effect_platform::http::reqwest HTTP, id_effect_axum hosting, id_effect_config, id_effect_logger,
  id_effect_rpc, id_effect_cli exit codes. Use when wiring binaries, HTTP servers, or
  platform capabilities at the application edge.
---

# id_effect Integration

**Part II ch7** + **Part III ch16** (CLI). Wire capabilities at the **edge** only.

**Prerequisite**: `id_effect-capabilities`.

## Workspace crates

| Crate | Role |
|-------|------|
| `id_effect` | Core |
| `id_effect_tokio` | `run_async` on Tokio runtime |
| `id_effect_platform` | FS, process, platform I/O capabilities |
| `id_effect_platform::http::reqwest` | HTTP client capability |
| `id_effect_axum` | `run_with_caps`, Axum host helpers |
| `id_effect_tower` | Tower service integration |
| `id_effect_config` | Figment/config providers |
| `id_effect_logger` | `EffectLogger` capability |
| `id_effect_rpc` | RPC boundaries |
| `id_effect_workflow` | Durable workflow spike |
| `id_effect_cli` | Exit codes, `run_main` helpers |

## Tokio bridge

```rust
// Domain stays sync Effect descriptions where possible
run_async(program).await?;
```

Use `from_async` when wrapping existing async Rust — keep `~` outside the `async move` block.

## HTTP server (Axum)

```rust
// run_with_caps at router/handler edge
// Handlers declare caps!(…) requirements; server provides Live providers
```

See book ch07-08 and `id_effect_axum` examples.

## Config & secrets

- **`id_effect_config`** — load config via providers, not `std::env` scattered in domain.
- CLI flags → config providers (ch16-02); use `Secret` types for sensitive values.

## Logging

Inject **`EffectLogger`** via capability; log inside `effect!` with `~logger.info(…)`.

## CLI binaries

```rust
// Map Exit → process exit code
id_effect_cli::exit_code_for_exit(&exit)
```

See ch16-01 for cause → exit code table.

## Pattern: thin main

```rust
fn main() {
    let code = run_main(|| {
        run_with([provide!(ConfigLive), provide!(…)], app())
    });
    std::process::exit(code);
}
```

Domain logic never constructs HTTP clients or opens files directly — declare `caps!(…)` and provide at `main`.

## Next

- Testing integration: [id_effect-testing](../id_effect-testing/SKILL.md)
