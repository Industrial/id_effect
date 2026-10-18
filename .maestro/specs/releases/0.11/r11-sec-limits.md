---
title: 'E11.2 — Load shedding and O(1) rate-limit eviction: r11-sec-limits'
slug: r11-sec-limits
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'M5: a LoadShed layer caps in-flight requests (default derived from available parallelism, configurable in HostConfig) and answers 503 Problem Details with Retry-After when full, without queueing; server_layer installs it by default (no such layer exists in crates/id_effect_axum/src/middleware/ today); disabling it is dangerous_no_load_shedding() (posture).'
  - 'M8: MemoryRateLimitStore eviction is O(1) amortized (LRU list or clock ring) and the store is sharded (no full scan as at middleware/rate_limit.rs:138-145; DEFAULT_MAX_KEYS at :96 kept).'
  - 'Regression tests `m5_load_shed_rejects_over_capacity_with_503`, `m5_default_server_stack_has_concurrency_cap`, `m8_eviction_does_not_scan_all_keys` (visit counter under a test hook), `m8_eviction_evicts_least_recently_used`, `m8_sharded_store_under_contention`.'
  - 'Gate: `cargo nextest run -p id_effect_axum --all-features`; `bash scripts/bench-run.sh --only id_effect_axum:gate_request` (no budget regression)'
  - 'Owns: `crates/id_effect_axum/src/middleware/{rate_limit.rs,load_shed.rs,mod.rs}`, `crates/id_effect_axum/src/server/{bootstrap.rs,lifecycle.rs}`, `crates/id_effect_axum/tests/limits.rs`'
---

# r11-sec-limits

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.2 — Secure by default" (M5, M8). The
release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: 0.10.1 r101-http (rate_limit.rs, lifecycle.rs connection semaphore) and
r11-sec-adr0013.
