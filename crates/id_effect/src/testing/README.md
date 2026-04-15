# `testing` — Stratum 16: test harness utilities

**Deterministic tests** and leak detection: [`run_test`](test_runtime.rs), [`run_test_with_clock`](test_runtime.rs), [`assert_no_leaked_fibers`](test_runtime.rs), [`assert_no_unclosed_scopes`](test_runtime.rs), plus [`SnapshotAssertion`](snapshot.rs) builders for golden-style assertions.

## What lives here

| Module | Role |
|--------|------|
| `test_runtime` | Entrypoints that wire runtime + clock + bookkeeping for tests. |
| `snapshot` | Structured snapshot / assertion helpers for integration outputs. |

## What it is used for

- **Unit/integration tests** that need a full `Effect` interpreter without copying boilerplate from `main`.
- **Guarding** against fiber or scope leaks in CI (especially after refactors to `Scope` / `FiberHandle`).
- **Stable** golden tests via snapshots where text/JSON output is compared.

## Best practices

1. **Prefer `run_test_with_clock`** when timing-sensitive code paths need control ([`scheduling::TestClock`](../scheduling/README.md)).
2. **Call** leak assertions in teardown of tests that spawn fibers or open scopes.
3. **Keep snapshots focused** — small, deterministic inputs; avoid flaky ordering from hash iteration without sorting.
4. **Follow** repo `TESTING.md` for naming (`when_*`, BDD-style) and `rstest` tables.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 16 (and repo `TESTING.md`).
- [`runtime`](../runtime/README.md) — what `run_test*` wraps.
- [`scheduling`](../scheduling/README.md) — `TestClock` integration.
