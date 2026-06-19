---
name: id_effect-platform
description: Platform capabilities — HttpClient, FileSystem, ProcessRuntime in id_effect_platform. Use when editing crates/id_effect_platform, platform Maestro missions, or migrating from id_effect_reqwest.
---

# id_effect-platform

## When to use

- Editing `crates/id_effect_platform` (HTTP, FS, process, URI)
- `platform-foundation` or Phase A parity work
- Migrating examples from `id_effect_reqwest` to portable HTTP

## Key APIs

- HTTP: `HttpClientKey`, `execute`, `execute_stream`, `provide_reqwest_http_client`
- FS: `FileSystemKey`, `read`, `exists`, `LiveFileSystem`, `TestFileSystem`
- Process: `ProcessRuntimeKey`, `spawn`, `spawn_wait`, `child_kill`, `child_wait`

## Related skills

- `id_effect-integration` — Tokio runtime, crate wiring
- `id_effect-streams` — `execute_stream` body as `Stream<u8>`

## Docs

- [ROADMAP](../../../docs/platform/ROADMAP.md)
- [Reqwest migration](../../../docs/platform/REQWEST_MIGRATION.md)
- Book: `crates/id_effect/book/src/part6/ch26-00-platform-introduction.md`
