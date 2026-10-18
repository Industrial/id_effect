---
title: 'E11.3 — SHA-pinned actions, pinned toolchains, least privilege, Cargo.lock: r11-sc-pin-permissions'
slug: r11-sc-pin-permissions
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'S1: every `uses:` in ci.yml, audit.yml, docs.yml and dependabot-auto-merge.yml is pinned to a full 40-character commit SHA with a `# vX.Y.Z` comment (today tags, e.g. actions/checkout@v7 at ci.yml:25); dtolnay/rust-toolchain@stable / @master (ci.yml:195, :134) replaced by a pinned SHA with an explicit toolchain input; rust-toolchain.toml pins the default toolchain; the moon installer `curl … | bash` (ci.yml:75, docs.yml:26) replaced by a pinned release download checked against its sha256; .github/dependabot.yml keeps the SHA pins updated.'
  - 'S2: every workflow it owns has a top-level `permissions: contents: read` (or `{}`) with job-level grants only where needed; dependabot-auto-merge.yml no longer approves its own PRs (`gh pr review --approve` at :27 removed) and only enables auto-merge for patch/minor updates after required checks and a CODEOWNERS review.'
  - 'S5: Cargo.lock is committed (.gitignore:71 entry removed, 0.11.0.md D12) and CI builds use `--locked`; the minimal-versions job stays.'
  - 'pg-integration job gains `-p id_effect_workflow` (for E7.5).'
  - 'scripts/check-workflows.sh fails on an unpinned `uses:`, a workflow without top-level permissions, or `curl … | sh/bash`, has --self-test cases `s1_every_action_pinned_by_sha`, `s2_every_workflow_least_privilege`, `s2_dependabot_does_not_self_approve`, and runs as a step in ci.yml over all of .github/workflows (other leaves'' new workflow files must pass it).'
  - 'Gate: `bash scripts/check-workflows.sh --self-test && bash scripts/check-workflows.sh`; `cargo build --workspace --locked`; `actionlint` on the owned workflows'
  - 'Owns: `.github/workflows/{ci.yml,audit.yml,docs.yml,dependabot-auto-merge.yml}`, `.github/dependabot.yml`, `rust-toolchain.toml`, `.gitignore`, `Cargo.lock`, `scripts/check-workflows.sh`'
---

# r11-sc-pin-permissions

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.3 — Supply chain and badges" (S1, S2, S5).
The release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: the wave-1 head. Cargo.lock becomes a shared append-only file: every later leaf
regenerates it on rebase. r11-w3-ledger edits the reference-ledger job in ci.yml in wave 3.
