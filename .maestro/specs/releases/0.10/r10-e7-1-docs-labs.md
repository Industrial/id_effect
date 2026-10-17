---
title: 'E7.1 — Jobs, `id_effect_jobs`: r10-e7-1-docs-labs'
slug: r10-e7-1-docs-labs
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Chapter + example + integration test cover #2–#6 #8; labs crate removed (no apalis/obix/job/rdkafka anywhere in `cargo metadata` of the root and labs workspaces); `grep -rniw stub crates/id_effect_jobs crates/id_effect_messaging` finds nothing; the labs CI job runs without `--exclude`. **Blocked by `r10-e7-2-outbox-inbox`** (msg.brokers#8 must exist before ObixInbox goes).'
  - 'Gate: `cargo test --doc -p book_snippets`; `bash scripts/tier-conformance.sh id_effect_jobs`; labs CI job'
  - 'Owns: `book/src/part6/ch31-00-async-messaging.md` (de-labs), `book/src/part6/ch31-01-jobs.md`, `SUMMARY.md` line, `crates/id_effect_jobs/examples/010_jobs.rs`, `crates/id_effect_jobs/tests/pg_queue.rs`, `labs/id_effect_jobs/**` (removed), `labs/Cargo.toml`, `.github/workflows/ci.yml` labs job (`:301-306`)'
---

# r10-e7-1-docs-labs

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.1 — Jobs, `id_effect_jobs` (L, wave 1 core → wave 2) · msg.jobs #2 #3 #4 #5 #6 #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
