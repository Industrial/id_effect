---
title: 'E8.2 — gen_statem parity: r11-e8-2-statem'
slug: r11-e8-2-statem
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Additive Statem behaviour in id_effect_fsm (the Copy-bound Fsm trait at crates/id_effect_fsm/src/definition.rs:44 is unchanged): fault.fsm#3 states may carry data (no Copy bound) and a separate Data value travels with the machine; handle_event(event, state, data, ctx) returns Transition::{Next(state, data, actions), Keep(data, actions), Stop(reason)}.'
  - 'fault.fsm#4 actions StateTimeout(d, msg) (cancelled on any state change), EventTimeout(d, msg) (cancelled by any event), GenericTimeout(name, d, msg), built on r11-e8-1-timers-hibernate; fault.fsm#5 Postpone: a postponed event is re-delivered after the next state change, in arrival order; also Reply and NextEvent actions; an optional state-enter callback runs on every state change (strengthens #6); runs as a supervised process (StatemProcess::start / start_linked).'
  - 'Tests `statem_state_carries_data`, `postponed_event_redelivered_after_state_change`, `state_timeout_cancelled_on_transition`, `event_timeout_cancelled_by_any_event`, `enter_action_runs_on_every_state_change`, `statem_under_supervisor_restarts`; example statem_door_lock.rs; book part5/ch19-05-statem.md (row level documented, 8/8 at cap 89).'
  - 'Gate: `cargo nextest run -p id_effect_fsm --all-features`; `cargo test -p id_effect_fsm --examples`'
  - 'Owns: `crates/id_effect_fsm/src/{statem.rs,lib.rs,error.rs}`, `crates/id_effect_fsm/tests/statem.rs`, `crates/id_effect_fsm/examples/statem_door_lock.rs`, `crates/id_effect/book/src/part5/ch19-05-statem.md`'
---

# r11-e8-2-statem

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E8.2 — gen_statem parity". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r11-e8-1-timers-hibernate (send_after and cancel_timer).
