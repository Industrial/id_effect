---
title: 0.8 E2.1h — remove the streaming expects; fix the 0.7 queue/channel/retry perf regressions
slug: r08-e2-1h-hygiene
mode: light
work_type: maintenance
risk_class: medium
version: 1
acceptance_criteria:
  - 'FoldChannel::from_sink (streaming/channel.rs:306) and Sink::zip (streaming/sink.rs:121-126) no longer panic: a FoldSink newtype (or Result-returning API) makes a non-fold sink a compile error or a typed error; the two should_panic tests in tests/stream_channel.rs are replaced; rg "\.expect\(" crates/id_effect/src/streaming returns nothing; MIGRATION-0.8 entry.'
  - 'Queue::offer/take (coordination/queue.rs:239-313) take a synchronous fast path when there is room/an item and fall back to the async path only when full/empty; take avoids the Vec allocation.'
  - 'retry_loop (scheduling/schedule.rs:501-525) no longer forces a fiber per run for count-only schedules (no clock read unless the schedule needs time), or the profile shows another dominant cost and that is fixed.'
  - 'bench-compare against docs/benchmarks/v0.7.json: coord/queue_bounded_10k and coord/channel_spsc_10k <= 1.25x v0.6, phase0/bench_schedule_retry_overhead <= 2x v0.6; the three ids added as budget gates in docs/benchmarks/gates.toml; bench-compare.sh --self-test passes; findings in docs/benchmarks/REPORT-0.8.md (draft section).'
non_goals:
  - Sweeping every expect in the workspace (counted and reported only).
---

# 0.8 E2.1h — hygiene

No scorecard row; quality bar for 0.8 (PLAN-OF-PLANS E2.1). Release spec: `.maestro/specs/releases/0.8.0.md`.
