---
title: 'T12 carry (F21) — EventLog head read and subscriptions "from now": r13-f-eventlog-head'
slug: r13-f-eventlog-head
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'F21: `EventLog::head(&self) -> Effect<u64, EventStoreError>` in crates/id_effect_events/src/subscription.rs (trait at `:42`) as a provided method (pages `read_all` to the end, so existing implementors compile), overridden in O(1) by the SQL journal (sql_journal.rs) and the in-memory store (event_store.rs).'
  - 'crates/id_effect_rpc/src/subscription/events.rs gains `Subscription::from_now()` starting at `head`; no cursor still means "from `starting_after`" (unchanged default), now documented in book ch33-08.'
  - 'Tests `head_is_last_global_position` (memory and `<pg>`), `head_default_impl_matches_override`, `subscription_from_now_skips_history`.'
  - 'Tier: integration (id_effect_events, id_effect_rpc). Secure by default: no new surface.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_events --all-features --summary-only --fail-under-regions 96` and the same for id_effect_rpc.'
  - 'Gate: `cargo nextest run -p id_effect_events -p id_effect_rpc --features events`; `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_events --test head`'
  - 'Owns: `crates/id_effect_events/src/{subscription.rs,sql_journal.rs,event_store.rs}` (the `head` method), `crates/id_effect_rpc/src/subscription/events.rs`, `crates/id_effect_events/tests/head.rs`, one paragraph of `crates/id_effect/book/src/part6/ch33-08-subscriptions.md`'
---

# r13-f-eventlog-head

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F21). The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: the v0.12.0 tag. Fills a free slot; nothing depends on it.
