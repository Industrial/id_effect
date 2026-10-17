---
title: 'E6.4 — Logging: r10-e6-4-redaction-sampling'
slug: r10-e6-4-redaction-sampling
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`RedactionPolicy` (field names, globs, `Secret<T>` values) applied once in `emit_record` before fan-out (#7); `SamplingBackend` (ratio per level + token-bucket rate limit, counts dropped lines) (#8); `LogRecord` gains `target` and becomes `#[non_exhaustive]` (MIGRATION-0.10). First integration tests `redacted_field_never_reaches_any_backend`, `rate_limit_drops_and_counts`.'
  - 'Gate: `cargo nextest run -p id_effect_logger --all-features`'
  - 'Owns: `crates/id_effect_logger/src/{lib.rs,pipeline.rs,redact.rs,sample.rs}`, `crates/id_effect_logger/tests/pipeline.rs`, `book/src/part6/ch27-00-observability.md` (section)'
---

# r10-e6-4-redaction-sampling

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E6.4 — Logging (S, wave 1) · obs.logging #7 #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
