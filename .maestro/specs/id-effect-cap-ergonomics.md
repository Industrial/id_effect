---
title: id_effect Cap DI Ergonomics
slug: id-effect-cap-ergonomics
mode: heavy
work_type: initiative
risk_class: high
version: 1
acceptance_criteria:
  - CapProjectAt index projection for CapList arities 1 through 8
  - CapBind cap_into_bind binds inner single-key effects at any index without zoom_env
  - CapWidenSecond deleted; prefix-only CapBind paths removed
  - effect macro supports tilde Key capability lookup sugar; require remains valid
  - effect macro accepts implicit r and synthesizes caps from body scan
  - trybuild ui corpus covers tilde Key and implicit-r misuse
  - book Part 2 ch04 examples and id_effect skill use target syntax
  - cap projection proptest passes
  - workspace tests clippy mdbook pass before publish
non_goals:
  - Multi-key combinatorial subset CapBind
  - Function attribute effect for return-type R inference
  - caps macro key-order normalization
  - Semver fork; pre-publish 0.3.0 only
---

# id_effect Cap DI Ergonomics

Finish capability DI ergonomics before first publish. See ADR 0005.
