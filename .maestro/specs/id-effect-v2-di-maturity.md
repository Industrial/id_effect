---
title: id_effect v2 DI Maturity
slug: id-effect-v2-di-maturity
mode: heavy
work_type: initiative
risk_class: high
version: 1
acceptance_criteria:
  - CapList const-generic replaces CapEnv1 through CapEnv6; caps macro supports arbitrary arity
  - compile-time caps enforcement via effect require tracking; require env K deleted
  - all workspace providers use capability attribute and ProviderSpec derive; define_capability deleted
  - logger config reqwest migrated off IntoBind to Needs and require
  - public Effect signatures use caps not bare Env
  - axum tokio E2E reference example and book chapter
  - named-variant primary replica provider example and tests
  - effectful provider cookbook example
  - fiber request-scoped capability overrides and axum middleware
  - optional refreshable shared providers in graph per ADR 0004
  - capability-set subtyping widening for Effect R parameter
  - config ambient.rs deleted; config_desc uses scoped Env only
  - context layer modules deleted; HasTag Matcher relocated; Effect provide removed
  - trybuild ui corpus at least 12 migration misuse cases
  - book Part 2 final purge; legacy layer examples archived
  - id-effect-diagnose reads provider manifests with JSON CI mode
  - capability error diagnostics with call-site context and snapshots
  - proptest fuzz for CapabilityGraph invariants
  - id_effect 3.0.0 and id_effect_platform 4.0.0 release with migration guide
non_goals:
  - Backwards compatibility shims or deprecation periods
  - CapEnv alias types alongside CapList
  - di_internal HList retention for macros
  - Public Effect with concrete Env for multi-capability handlers
  - 2.x intermediate semver release
---

# id_effect v2 DI Maturity

Clean-break adoption of v2 DI: mandatory compile-time caps, delete all v1/legacy APIs, production provider patterns, expanded quality gates, and coordinated id_effect 3.0.0 release.
