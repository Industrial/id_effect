---
name: id_effect-platform
description: Platform capabilities — HttpClient, FileSystem, ProcessRuntime in id_effect_platform. Use when editing crates/id_effect_platform, platform Maestro missions, or migrating from id_effect_platform::http::reqwest.
---

# id_effect-platform

## When to use

- Editing `crates/id_effect_platform` (HTTP, FS, process, URI)
- `platform-foundation` or Phase A parity work
- Using `id_effect_platform::http::reqwest` to `id_effect_platform::http::reqwest`

## Key APIs

- HTTP (portable): `HttpClientKey`, `execute`, `execute_stream`, `provide_reqwest_http_client`
- HTTP (reqwest): `http::reqwest::{send, json_schema, provide_reqwest_client, provide_reqwest_pool}`
- FS: `FileSystemKey`, `read`, `exists`, `LiveFileSystem`, `TestFileSystem`
- Process: `ProcessRuntimeKey`, `spawn`, `spawn_wait`, `child_kill`, `child_wait`

## Related skills

- `id_effect-integration` — Tokio runtime, crate wiring
- `id_effect-streams` — `execute_stream` body as `Stream<u8>`

## Docs

- [ROADMAP](../../../docs/platform/ROADMAP.md)
- [Reqwest migration](../../../docs/platform/REQWEST_MIGRATION.md)
- Book: `crates/id_effect/book/src/part6/ch26-00-platform-introduction.md`
