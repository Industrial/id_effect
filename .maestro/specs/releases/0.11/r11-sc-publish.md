---
title: 'E11.3 — Trusted publishing, SLSA L3 provenance, verified attestations: r11-sc-publish'
slug: r11-sc-publish
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'S3: crates.io trusted publishing over GitHub OIDC (rust-lang/crates-io-auth-action, SHA-pinned); `CARGO_REGISTRY_TOKEN` (publish.yml:113) removed from every workflow. Configuring the trusted publisher and deleting the secret are owner actions (0.11.0.md D15) recorded with dates in the leaf evidence.'
  - 'S5 (0.11.0.md D13): the build job runs `cargo package --locked` for every crate from `scripts/validate-crate-packaging.sh --print-order`; SLSA Build L3 provenance is produced by the slsa-framework/slsa-github-generator generic reusable workflow over those .crate files and the SBOMs (replacing the re-download at publish.yml:160-166 and the attestation at :169); `cargo publish` publishes the same tree.'
  - 'Verify job after publish: for every crate, download it from static.crates.io, check its sha256 equals the attested subject digest and run slsa-verifier verify-artifact; the release fails on any mismatch (exit KPI "a verified attestation per crate", test `s5_attested_digest_matches_published_crate`).'
  - 'S6: publish refuses an unsigned tag (`s6_release_requires_signed_tag`); top-level permissions read, job-level id-token/attestations only where needed; publishes lints/id_effect_lints (r11-e1-9-dylint) with its pinned nightly after the workspace crates; docs/RELEASING.md documents the flow and the owner steps.'
  - 'Gate: `bash scripts/check-workflows.sh`; `! grep -rn CARGO_REGISTRY_TOKEN .github`; `actionlint .github/workflows/publish.yml`; a workflow_dispatch run with dry_run=true recorded (run URL in the evidence)'
  - 'Owns: `.github/workflows/publish.yml`, `.github/scripts/publish-crate.sh`, `docs/RELEASING.md`'
---

# r11-sc-publish

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.3 — Supply chain and badges" (S3, S5, S6).
The release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: the wave-1 head; its gate uses scripts/check-workflows.sh from
r11-sc-pin-permissions (land that first or run the gate after it). Rollback: the previous
publish.yml (token + attest-build-provenance) is one revert away, and the token secret is deleted
only after the first successful trusted publish.
