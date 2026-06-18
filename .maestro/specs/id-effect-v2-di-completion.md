---
title: id_effect v2 DI Completion
slug: id-effect-v2-di-completion
mode: heavy
work_type: initiative
risk_class: high
version: 1
acceptance_criteria:
  - caps macro expands to typed CapabilitySet with runtime verify at run_with boundary
  - effect macro supports require Key without explicit env parameter
  - capability attribute generates internal keys
  - ProviderSpec derive reduces boilerplate
  - named capability variants and generic env cells tested
  - effectful providers run in topo order before app effect
  - Env scoped supports request overrides and axum run_with bridge
  - provider lifecycle shutdown hooks run reverse-topo
  - providers dev prod bundle macro validates graph
  - EffectInterface removed from algebra interface module
  - HttpClientKey hidden from public platform API
  - config ambient deprecated in favor of scoped Env
  - mock_capability macro for test doubles
  - id-effect-diagnose CLI prints CapabilityGraph diagnostics
  - id_effect_lint v2 DI rules enforced
  - trybuild ui corpus for v1 removal and v2 misuse
  - mdBook consolidated and layer examples retired
  - cargo test workspace passes
non_goals:
  - Full Effect.ts parity beyond DI
  - Runtime reflection-based injection
  - Changing Effect error algebra or fiber runtime
  - Deleting pub(crate) HList engine in context/layer
---

# id_effect v2 DI Completion

Complete the v2 capability DI type-system promise, macro ergonomics, advanced provider semantics, ecosystem unification, and quality gates left after the v2.0.0 clean break.
