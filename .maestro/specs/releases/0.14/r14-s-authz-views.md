---
title: 'E13.1 Sweep — authorization helpers in maud views (can? for id_effect_axum ui): r14-s-authz-views'
slug: r14-s-authz-views
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'sec.authz#7: feature `ui` of id_effect_authz (new: `["axum", "id_effect_axum/ui"]`) adds crates/id_effect_authz/src/view.rs. Policies are effects (`Policies::evaluate`, crates/id_effect_authz/src/registry.rs:181) and maud rendering is synchronous, so the helper is two-phase: in the handler, `Abilities::check(&policies, &principal).can("post.edit", &post).can_on_each("post.delete", &posts)` evaluates every needed decision through the registry (recorded as `DecisionEvent`s when events are attached, src/audit.rs:67) and yields `Abilities`; in the view, `abilities.can("post.edit", &post) -> bool` and `abilities.when("post.edit", &post, html! { .. }) -> Markup` (implements `maud::Render`, re-exported by crates/id_effect_axum/src/ui/mod.rs:70-71) read the result. A pair that was never checked answers `false` (deny by default) and is listed by `Abilities::unchecked()`, and in debug builds logs a warning naming the policy. Resources are identified by `ResourceKey` (type name + an id the app gives through a small `ViewResource` trait). Today crates/id_effect_authz has guards for routes (src/guard.rs:82) and policies for code, nothing for templates. Row sec.authz 88 → 100.'
  - 'Tests: crates/id_effect_authz/tests/views.rs `can_renders_markup_only_when_the_policy_allows`, `unchecked_ability_denies_by_default_and_is_reported`, `abilities_evaluate_through_named_policies_and_record_events`, `abilities_respect_tenant_scope` (a resource of another tenant is denied, as src/tenant.rs scopes it), `anonymous_principal_sees_no_guarded_links`, `can_on_each_checks_every_row_once`.'
  - 'Book: new section "## In views and templates" in `crates/id_effect/book/src/part6/ch30-02-authorization.md` (before "## Data-layer scoping", :312), cross-linked from part6/ch29-10-server-rendered-ui.md by one sentence; example `crates/id_effect_authz/examples/030_view_helpers.rs` (required-features `ui`, `testing`).'
  - 'Tier: platform (id_effect_authz; Ledger consumer). Secure by default (ADR 0013): R1 deny by default for every unchecked or failed decision (a policy error renders as denied, never as allowed); hiding a link is not access control: the chapter and the rustdoc say the route guard still decides, and `abilities_*` tests run beside a guarded route; no new HTTP surface.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_authz --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_authz --all-features`; `cargo clippy -p id_effect_authz --all-targets --all-features -- -D warnings`; `bash scripts/tier-conformance.sh id_effect_authz`'
  - 'Owns: `crates/id_effect_authz/src/view.rs` (new), `crates/id_effect_authz/tests/views.rs` (new), `crates/id_effect_authz/examples/030_view_helpers.rs` (new), `crates/id_effect_authz/Cargo.toml` (feature `ui`, `[[example]]` entry), `crates/id_effect/book/src/part6/ch30-02-authorization.md` (the new section), the one cross-link sentence in `crates/id_effect/book/src/part6/ch29-10-server-rendered-ui.md`, the kit `ui` feature line in `crates/id_effect_kit/Cargo.toml` (adds `"id_effect_authz?/ui"`); append-only: `crates/id_effect_authz/src/lib.rs` (mod and `pub use` lines), `CHANGELOG.md`'
---

# r14-s-authz-views

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: the wave-1 merge (no wave-1 leaf owns its files). Blocks nothing in later waves
except the level audit and re-score; r14-p-ledger or Warden may use it in a view.

Defaults the owner may overturn: the helper lives in id_effect_authz behind `ui`, not in
crates/id_effect_axum/src/ui (axum has id_effect_authz only as a dev-dependency, Cargo.toml:180, and
must not gain it as a normal one), so this leaf edits no file of id_effect_axum and cannot collide with
r14-s-web-uploads; decisions are precomputed in the handler (no effect runs during rendering). Not
done here: a macro DSL inside `html!`, authorization helpers for Leptos (the recipe).
