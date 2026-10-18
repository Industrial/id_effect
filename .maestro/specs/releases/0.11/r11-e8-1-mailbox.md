---
title: 'E8.1 — Bounded mailboxes and selective receive: r11-e8-1-mailbox'
slug: r11-e8-1-mailbox
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'M10 (0.11.0.md D17): every Mailbox is bounded (crates/id_effect_process/src/mailbox.rs:41 today says "Unbounded"), default 10,000 user messages, configurable per spawn; system messages (exit signals, monitor downs, kill) bypass the bound; a full mailbox makes local sends fail with SendError::Full (SendError becomes #[non_exhaustive], msg.rs:114) and Addr::send_now return false (addr.rs:66-71); remote delivery to a full mailbox (crates/id_effect_node/src/registry.rs:208,220,234) faults the remote call and drops + counts a remote cast.'
  - 'fault.actors#7 selective receive: ProcessCtx::receive(matcher, timeout) returns the first message the matcher accepts and keeps every skipped message in arrival order for the normal loop; the scan is bounded by the mailbox capacity; a timeout yields a typed error.'
  - 'Regression tests `m10_bounded_mailbox_rejects_when_full`, `m10_remote_send_to_full_mailbox_faults`, `m10_system_messages_bypass_bound`; feature tests `selective_receive_keeps_order_of_skipped_messages`, `selective_receive_times_out`. MIGRATION-0.11 section for SendError.'
  - 'Gate: `cargo nextest run -p id_effect_process -p id_effect_node --all-features`; `cargo clippy -p id_effect_process --all-targets --all-features -- -D warnings`'
  - 'Owns: `crates/id_effect_process/src/{mailbox.rs,addr.rs,msg.rs,node.rs,lib.rs}`, `crates/id_effect_node/src/registry.rs`, `crates/id_effect_process/tests/{mailbox_bounds.rs,selective_receive.rs}`'
---

# r11-e8-1-mailbox

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E8.1 — Actors" and "Findings → leaves" (M10).
The release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r11-sec-adr0013 (posture registry, if an unbounded opt-out is offered it must be
`dangerous_unbounded()` and recorded). r11-e8-1-timers-hibernate follows in wave 2 on the same files.
