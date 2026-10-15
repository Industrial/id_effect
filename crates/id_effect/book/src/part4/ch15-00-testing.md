# Testing — Effects Are Easy to Test

Testing async code is usually painful: real time makes tests slow and flaky, shared state leaks
between tests, and mocking dependencies takes framework magic.

Effect programs are different. An `Effect` is a description of what to do, not the doing itself,
so the test decides how it runs:

- **Run it at the edge.** `eff.block_on()` returns the full `Exit` (success, typed failure,
  defect or interruption), and you assert on it like any value.
- **Provide fakes as capabilities.** `R` is a typed list of capabilities. A test provides plain
  values (`provide`, `provide_env(hlist![..])`, `Layer::succeed`) instead of the production ones;
  forgetting one is a compile error, not a runtime surprise.
- **Control time.** Sleeps, timeouts, retries and schedules go through the executor's clock.
  Run the effect on a `TestExecutor` and its virtual clock moves only when the test calls
  `advance`, so an hour-long backoff takes microseconds.
- **Control randomness.** `TestRandom` is a seeded, feedable `Random` capability.

## What This Chapter Covers

- **Running effects in tests**: `block_on`, asserting on `Exit` ([next section](./ch15-01-run-test.md))
- **Virtual time with `TestExecutor`**: deterministic sleeps, timeouts and retries ([ch15-02](./ch15-02-test-clock.md))
- **Mocking services**: test doubles as capabilities and layers ([ch15-03](./ch15-03-mocking.md))
- **Property testing**: `proptest` over effect programs ([ch15-04](./ch15-04-property-testing.md))

The kernel's own suites in `crates/id_effect/tests/` (`schedule.rs`, `race_timeout.rs`,
`layer.rs`, `macros.rs`, …) use exactly these techniques and are good further reading.
