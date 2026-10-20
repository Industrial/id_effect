---
title: 'E11.4 part 1 — cargo-fuzz targets with ClusterFuzzLite: r11-fuzz-ci'
slug: r11-fuzz-ci
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'A fuzz/ cargo-fuzz workspace (outside the main workspace, one pinned nightly date, 0.11.0.md D20) with at least 8 targets over public decoders whose input formats do not change in 0.11, chosen from: Schema decode from JSON (id_effect), typed-route path and query decode and form-body decode (id_effect_axum route/decode.rs), cron expression parse (id_effect scheduling), config profile TOML merge (id_effect_config), JWT header/claims decode (id_effect_auth jwt), JWKS document parse (id_effect_auth jwks), otpauth URI parse (id_effect_auth totp). The implementer confirms each target reaches only public API.'
  - 'Seed corpora committed; .clusterfuzzlite/ (Dockerfile, build.sh, project.yaml); .github/workflows/cflite_pr.yml (PR mode, 5 minutes per target, fails on a new crash) and cflite_batch.yml (nightly batch with stored corpus), SHA-pinned with top-level permissions read; docs/security/fuzzing.md (how to run, how a crash becomes an issue and a regression test).'
  - 'KPI: `ls fuzz/fuzz_targets/*.rs | wc -l` ≥ 8 at this leaf (≥ 10 after r11-e8-3-reliable-delivery adds mesh_frame and mesh_envelope); no open crash at exit.'
  - 'Gate: `cd fuzz && cargo +nightly-<pinned> fuzz build`; `for t in $(cargo +nightly-<pinned> fuzz list); do cargo +nightly-<pinned> fuzz run $t -- -max_total_time=60; done` with no crash; `bash scripts/check-workflows.sh`'
  - 'Owns: `fuzz/**`, `.clusterfuzzlite/**`, `.github/workflows/{cflite_pr.yml,cflite_batch.yml}`, `docs/security/fuzzing.md`'
---

# r11-fuzz-ci

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.4 part 1, E1.9, E2.6". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: the wave-1 head. r11-e8-3-reliable-delivery adds two targets in wave 2.
