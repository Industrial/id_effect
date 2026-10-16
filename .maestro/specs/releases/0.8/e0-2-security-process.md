---
title: 0.8 E0.2 — security process (SECURITY.md, cargo-deny, SBOM, provenance)
slug: r08-e0-2-security-process
mode: light
work_type: maintenance
risk_class: high
version: 1
acceptance_criteria:
  - 'SECURITY.md: private reporting via GitHub security advisories, response times, supported-versions table and support window, advisory (GHSA/RUSTSEC) publication process.'
  - 'deny.toml (licenses allow-list derived from cargo deny check licenses on the main workspace, bans, sources; carries the RUSTSEC-2023-0071 ignore from .cargo/audit.toml); cargo deny check exits 0; a deny job in .github/workflows/ci.yml listed in ci-pass.needs; moon task deny; cargo-deny in devenv.nix.'
  - 'publish.yml: a CycloneDX SBOM (cargo cyclonedx) per release attached to the GitHub release, and actions/attest-build-provenance over the packaged .crate files and SBOMs with permissions id-token: write, attestations: write scoped to that job; cargo-cyclonedx in devenv.nix; actionlint clean on changed workflows.'
non_goals:
  - Crates.io trusted publishing migration (optional follow-up).
  - Labs workspace deny policy.
---

# 0.8 E0.2 — security process

Moves sec.supply 2/8 -> 6/8 (#3 deny, #4 SECURITY.md, #5 SBOM, #6 provenance; #8 support window
documented). Release spec: `.maestro/specs/releases/0.8.0.md`.
