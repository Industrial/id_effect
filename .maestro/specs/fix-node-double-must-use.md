---
title: Fix clippy double_must_use in id_effect_node async_trait traits
slug: fix-node-double-must-use
mode: light
work_type: maintenance
risk_class: medium
version: 1
acceptance_criteria:
  - 'On the CI-stable toolchain (clippy 0.1.99), cargo clippy -p id_effect_axum --all-targets --all-features -- -D warnings exits 0 (was 7 double_must_use errors from async-trait 0.1.89 expansions in id_effect_node).'
  - 'The fix is scoped: #[allow(clippy::double_must_use)] with a one-line reason on the four #[async_trait] trait definitions (Discovery, Connection, Listener, Transport); no public API change.'
  - 'The always-run pre-commit gate (devenv pre-commit -> ci-local affected) passes, so the commit is made without --no-verify.'
non_goals:
  - Replacing async_trait with native async fn in traits.
---

# Fix clippy double_must_use in id_effect_node

async-trait 0.1.89 emits `#[must_use]` on the generated trait methods, whose return type
(`Pin<Box<dyn Future>>`) is already must_use; clippy 0.1.99 (CI-stable pin) reports
`double_must_use` 7 times, which fails the pre-commit gate and CI clippy for every commit.
Found during roadmap-bootstrap (commits 61f3b78, 3af5d74 used --no-verify).
