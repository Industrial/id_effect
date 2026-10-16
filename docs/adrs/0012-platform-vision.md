# ADR 0012 — Platform vision

## Status

Accepted (2026-10-15). Mission `pln-mv9xenpd-d2lpkn` (roadmap bootstrap), leaf
`leaf-adr-0012`. Supersedes in part [ADR 0010](0010-crate-tiers.md): its vision
statement and its Rule 4. ADR 0010's tiers, Rules 1–3 and 5, and Amendments 1–3 stay
in force. The plan this ADR makes legal is
[`PLAN-OF-PLANS.md`](../../.maestro/specs/roadmap/PLAN-OF-PLANS.md); the numbers come from
the scorecard ([`docs/scorecard/METHOD.md`](../scorecard/METHOD.md)).

## Context

The 0.7.0 scorecard (method v1.0, baseline 2026-10-15) ranks id_effect against ten
enterprise stacks:

- **Enterprise index E 46.8, rank #11 of 11.** id_effect is last.
- **The runtime groups are near the leaders.** Runtime index RT 59.1 (#7); typed core
  86 (group average), concurrency 85, supervision 84, mesh 65–72. id_effect is the only
  non-BEAM entrant with OTP-grade supervision and distribution.
- **Batteries and trust are absent.** Data 34, security 29, messaging 28, developer
  experience 47, ecosystem and trust 14. 31 rows are red (< 40), and the first hour (K1)
  is not possible because there is no generator.

The gap is not in what id_effect does but in what it refuses to do. ADR 0010:39-42 fixes
the vision as "a typed effect runtime … It is not a batteries-included web framework",
and every crate has to justify itself against that sentence. ADR 0010 Rule 5 requires an
ADR amendment that names a tier before any new published crate, and Rule 2 promotes a
labs crate only when a real consumer depends on it. Every battery crate in the plan of
plans (sql, auth, jobs, mail, storage, console, …) would violate the vision, and the labs
crates the plan promotes (rpc, workflow, events, ai) have no external consumer to satisfy
Rule 2. Under ADR 0010 the goal is forbidden.

## Decision

### Vision

> **id_effect is the platform where the compiler checks your dependencies, errors and
> resources, the runtime supervises them, and one command gets you a production-shaped
> service.**

This replaces the ADR 0010 vision statement. Every crate has to justify itself against
this sentence. Every battery must be effect-native: a `Layer` to provide it, a typed
error channel, interruption-safe and supervisable, and testable with `TestExecutor`.

### Locked decisions

Copied from PLAN-OF-PLANS §3.1. They change only by ADR.

| # | Decision | Rationale |
|---|---|---|
| L1 | **Supersede the ADR 0010 vision with ADR 0012 "Platform vision".** id_effect becomes a batteries-included platform. ADR 0010's tiers stay; a **platform** tier is added for battery crates, using the integration-tier gates plus a reference-app consumer. | ADR 0010:39-42 forbids the goal; Rule 5 requires an amendment before any new crate. |
| L2 | **Effect-native batteries, built on proven Rust crates.** Integrate (sqlx, sea-query, lettre, object_store, openidconnect, jsonwebtoken, argon2, fluent, async-graphql); do not rewrite them. | The value is the typed effect surface, not reimplementing protocols. |
| L3 | **One front door:** the facade crate `id_effect_kit` (feature flags per battery, `prelude`) plus the `id-effect` CLI binary (`new`, `generate`, `dev`, `doctor`, `migrate`, `deploy`). | Coherence (45), scaffolding (4) and the first hour are the biggest DX gaps. |
| L4 | **Reference apps are the consumers.** `examples/reference/{ledger,relay,pipeline}` are CI-tested apps. They count as the "real consumer" for labs → platform promotion and for the ≥ 90 cap. | ADR 0010 Rule 2 (promotion needs a real consumer) blocked promotion; ADR 0012 amends Rule 4 (labs) so a reference app satisfies it. |
| L5 | **The scorecard is the release KPI.** Each release ships `docs/scorecard/results/<ver>.json` and `REPORT-<ver>.md`. Missing an index target by more than 1.0 requires a written carry-over. | The user's request: prove progress with numbers. |
| L6 | **Postgres first, then SQLite, then MySQL.** Jobs, sessions, outbox and workflows default to Postgres, with SQLite for dev and tests. | One excellent path beats three mediocre ones. |
| L7 | **UI: integrate, do not build.** Ship HTMX+maud and Leptos recipes and an admin generator. No in-house frontend framework (`web.ui` target 40). | Next.js and Phoenix own this; Rust UI frameworks exist. |
| L8 | **Hot code loading remains a non-goal.** Target `dev` auto-restart and zero-downtime rolling deploys instead (`fault.reload` target 55). | ADR 0009. |
| L9 | **Breaking changes are allowed until 1.0**, but each one needs a deprecation shim for one minor version and a migration-guide entry. `cargo-semver-checks` runs in CI from 0.8. | Stability (28) is a trust blocker. |
| L10 | **License: MIT** (owner decision 2026-10-15). A single `LICENSE` file, `license = "MIT"` in every manifest, and a DCO line in CONTRIBUTING. | It is the most widely accepted permissive license, and corporate legal reviews clear it routinely. |
| L11 | **Facade crate: `id_effect_kit`** (owner decision 2026-10-15). | Clear and searchable. |
| L12 | **Console built into the app** (owner decision 2026-10-15). `id_effect_console` is mounted by the service at `/_console` behind auth, server-rendered (axum + maud + htmx) with live updates, behind a feature flag. No separate SPA and no JavaScript toolchain in the repo. | Nothing extra to deploy, everything stays in Rust, and it follows the Phoenix LiveDashboard model. |

### Tiers

The ADR 0010 tiers are unchanged. One tier is added:

```toml
[package.metadata.id_effect]
tier = "platform"   # core | runtime | integration | platform | maintenance | labs | merge | example
```

| Tier | Published | Bar | Crates (target state) |
|---|---|---|---|
| `core` | yes | strict | unchanged (ADR 0010) |
| `runtime` | yes | strict | unchanged (ADR 0010) |
| `integration` | yes | standard | unchanged (ADR 0010) |
| `platform` | yes | standard + consumer | battery crates (PLAN-OF-PLANS §4): id_effect_auth, id_effect_authz, id_effect_jobs, id_effect_mail, id_effect_storage, id_effect_redis, id_effect_channels, id_effect_console, id_effect_graphql, id_effect_i18n, id_effect_test, and labs crates promoted through Rule 4 |
| `maintenance` | yes | frozen | unchanged (ADR 0010) |
| `labs` | **no** | builds only | unchanged (ADR 0010) |
| `merge` | transitional | none | unchanged (empty since ADR 0010 Amendment 3) |
| `example` | no | builds | unchanged (ADR 0010) |

The `platform` gates are:

- every `integration`-tier gate (README with a Rust example, examples, `deny(missing_docs)`,
  no crate-wide `allow(dead_code|unused…)`, no stub markers, the coverage floor);
- test density ≥ 1/80;
- **depended on by a CI-tested reference app under `examples/reference/`.**

`docs/QUALITY_BAR.md` lists them and `scripts/tier-conformance.sh` checks them. Each new
platform crate still needs its own ADR amendment naming the tier (ADR 0010 Rule 5); this
ADR does not pre-approve any crate.

### Rule 4 amended

ADR 0010 Rule 2 promotes a labs crate only when "a real consumer depends on it". This
ADR amends the labs rule (Rule 4) so that **a CI-tested reference app under
`examples/reference/` counts as a real consumer** for labs → platform promotion. The rest
of Rule 4 stands: labs crates stay out of `publish.yml`, the coverage floor and the
default CI matrix, and build in a separate CI job.

## Consequences

Positive:

- The plan of plans is legal under the repo's own governance: battery crates have a tier,
  and labs crates have a promotion path that does not wait on outside users.
- The quality bar does not drop. Platform crates meet the integration bar and must be
  exercised end to end by a reference app that CI runs.
- Progress is measured. The scorecard ([METHOD](../scorecard/METHOD.md)) is the release
  KPI (L5), so a battery that does not move a row is visible as such.

Negative:

- Scope grows a lot. The published surface, CI time and maintenance load all increase,
  which is the breadth-over-quality failure ADR 0010 was written to stop. The platform gates
  and the per-crate ADR amendment are the counterweight.
- Reference apps become load-bearing. A reference app that rots blocks promotions, and an
  app written only to promote a crate is a weak consumer; reviewers check that it is a
  realistic service.
- Heavy dependencies (database drivers, OIDC, object stores) come back into the workspace.
  Postgres-first (L6) and feature flags per battery (L3) limit, but do not remove, the CI
  weight.
- Trust (ecosystem 14) is not fixed by code. Even on plan, the target is #3 at 1.0
  (PLAN-OF-PLANS §1).

## Alternatives considered

- **Keep the ADR 0010 vision.** Rejected: E stays capped by absent batteries; the
  runtime lead alone does not lift the rank.
- **A separate batteries repository or umbrella project.** Rejected for the same reason
  ADR 0010 rejected a separate labs repository: core API changes would need coordinated
  edits across repositories, and the facade (L3) needs everything in one release train.
- **Wait for external consumers before promoting labs crates.** Rejected: it is the
  deadlock Rule 4 created. Reference apps are tested in CI, which an external consumer is
  not.
