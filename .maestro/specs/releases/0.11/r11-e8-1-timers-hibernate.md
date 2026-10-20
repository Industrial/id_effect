---
title: 'E8.1 — Timers, idle timeouts and hibernate: r11-e8-1-timers-hibernate'
slug: r11-e8-1-timers-hibernate
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'fault.actors#6: ProcessCtx::send_after(msg, d) -> TimerRef, Addr::send_after for callers outside the process, cancel_timer(TimerRef) -> bool (true means the message will never be delivered); timers run on the executor clock (TestExecutor can advance them) and die with the process; ProcessCtx::set_idle_timeout(d) delivers Info::Timeout once per idle period.'
  - 'fault.actors#8: Next::Hibernate(state) releases the mailbox buffers and calls an optional Process::hibernate hook, and the process resumes on the next message. Next (process.rs:41) and Info (msg.rs:40) become #[non_exhaustive] (MIGRATION-0.11 section).'
  - 'Tests `send_after_delivers_under_test_clock`, `cancelled_timer_never_delivers`, `timers_die_with_process`, `idle_timeout_fires_once_per_idle_period`, `hibernate_releases_buffer_and_resumes`; runnable examples for send_after and hibernate; book part7/ch37-01-timers-selective-receive.md covers #6, #7 and #8 (row level documented).'
  - 'Gate: `cargo nextest run -p id_effect_process --all-features`; `cargo test -p id_effect_process --examples`'
  - 'Owns: `crates/id_effect_process/src/{process.rs,cell.rs,timer.rs,dynamic.rs,supervisor.rs,msg.rs,lib.rs}`, `crates/id_effect_process/tests/timers.rs`, `crates/id_effect_process/examples/` (new send_after and hibernate examples), `crates/id_effect/book/src/part7/ch37-01-timers-selective-receive.md`'
---

# r11-e8-1-timers-hibernate

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E8.1 — Actors". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r11-e8-1-mailbox (same crate; msg.rs and lib.rs pass to this leaf). r11-e8-2-statem
builds its timeouts on these timers in wave 3.
