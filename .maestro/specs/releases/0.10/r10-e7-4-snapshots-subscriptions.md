---
title: 'E7.4 — Events, `id_effect_events`: r10-e7-4-snapshots-subscriptions'
slug: r10-e7-4-snapshots-subscriptions
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`SnapshotStore` + aggregate load = snapshot + tail (#6); global position column and `CatchUpSubscription` with a stored checkpoint, resuming after restart (#8). Tests `aggregate_loads_from_snapshot_plus_tail`, `subscription_resumes_from_checkpoint`. Chapter + example + integration test.'
  - 'Gate: same + `cargo test --doc -p book_snippets`; `bash scripts/tier-conformance.sh id_effect_events`'
  - 'Owns: `crates/id_effect_events/src/{snapshot.rs,subscription.rs}`, `book/src/part5/ch23-00-events-and-projections.md` (de-labs, `SUMMARY.md:151`), `crates/id_effect_events/examples/010_account.rs`, `crates/id_effect_events/tests/pg_events.rs`'
---

# r10-e7-4-snapshots-subscriptions

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.4 — Events, `id_effect_events` (M, wave 1) · data.events #6 #8 (+#1–#5 kept)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
