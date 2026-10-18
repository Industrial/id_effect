---
title: 'E11.3 — DCO enforcement, SECURITY.md, THREAT-MODEL.md: r11-sc-dco-docs'
slug: r11-sc-dco-docs
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'S6: .github/workflows/dco.yml fails a pull request when any commit lacks a Signed-off-by line (`s6_dco_check_rejects_unsigned_off_commit` as a fixture run); CONTRIBUTING.md (DCO section at :12-20) documents signing commits (SSH or GPG, or gitsign) and the signed-tag rule; enabling "require signed commits" on protected branches is an owner action (0.11.0.md D15) recorded with its date.'
  - 'S7 and sec.supply#7: SECURITY.md supported versions list 0.11.x and 0.10.x (90-day window), replacing 0.9.x at :35, and a "Security releases" section lists the 0.10.1 GHSA advisories (ids and links) as public security-release history.'
  - 'THREAT-MODEL.md: STRIDE per trust boundary (HTTP edge, authentication, authorization, mesh, console/operator surfaces, data/SQL/Redis, outbound HTTP, supply chain); each threat names its mitigation, the ADR 0013 rule (R1–R10) and the test or gate; L6 (rsa 0.9 RUSTSEC-2023-0071 via sqlx-mysql and openidconnect, ignored with a reachability reason at deny.toml:18) is an accepted risk with the reachability argument and a re-check each release.'
  - 'Gate: `bash scripts/check-workflows.sh`; `actionlint .github/workflows/dco.yml`; for each of R1..R10, `grep -c "R<n>" THREAT-MODEL.md` ≥ 1'
  - 'Owns: `.github/workflows/dco.yml`, `CONTRIBUTING.md`, `SECURITY.md`, `THREAT-MODEL.md`'
---

# r11-sc-dco-docs

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.3 — Supply chain and badges" (S6, S7, L6).
The release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: the wave-1 head (ADR 0013 rule ids). The GHSA ids exist only after 0.10.1 is on
crates.io and its advisories are published.
