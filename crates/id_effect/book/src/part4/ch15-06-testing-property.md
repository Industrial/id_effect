# First-Party Property Testing and Test Helpers

id_effect ships its own test helpers next to [`TestExecutor`](./ch15-02-test-clock.md):

| Helper | Feature | What it is for |
|---|---|---|
| `run_test`, `run_test_on` | — | Run an effect to its `Exit` on a `TestExecutor`, skipping through virtual time |
| `assert_exit_success`, `assert_exit_fail`, `assert_exit_die`, `assert_exit_interrupted`, `assert_exit_kind`, `assert_exit_eq`, `expect_exit_success`, `expect_exit_failure` | — | Assertions on `Exit` / `Cause` |
| `assert_golden`, `assert_golden_debug`, `assert_golden_exit`, `render_exit` | — | Golden-file (snapshot) tests |
| `testing::property` | `proptest` | proptest strategies for framework types and schemas, effect-aware properties, model-based testing |

The property module sits on [proptest](https://docs.rs/proptest), so failing cases are shrunk to
a minimal input and their seeds are persisted under `proptest-regressions/` and replayed first on
the next run.

```toml
[dev-dependencies]
id_effect = { version = "0.10", features = ["proptest"] }
proptest = "1"
```

## Running effects and asserting on their exit

`run_test` forks the effect as a root fiber on a fresh `TestExecutor` and drives it to
completion: it runs every ready task, then jumps the virtual clock to the next timer, until the
fiber is done. An hour of `sleep`, a `timeout` or a `retry` schedule costs no wall-clock time.
It panics on a deadlock (the fiber waits on something nothing will complete).

```rust
use std::time::Duration;

use id_effect::{Effect, Schedule, assert_exit_fail, assert_exit_success, run_test, sleep};
# use id_effect::fail;
# #[derive(Debug, PartialEq)]
# enum FetchError { Unavailable }
# fn fetch_flaky() -> Effect<u8, FetchError> { fail(FetchError::Unavailable) }

#[test]
fn slow_lookup_times_out() {
  let lookup: Effect<u8> = sleep(Duration::from_secs(60)).map(|()| 1);
  assert_exit_success(&run_test(lookup.timeout(Duration::from_secs(5))), &None);
}

#[test]
fn retries_then_gives_up() {
  let eff = Effect::retry(|| fetch_flaky(), Schedule::spaced(Duration::from_secs(1)).intersect(Schedule::recurs(3)));
  assert_exit_fail(&run_test(eff), &FetchError::Unavailable);
}
# let lookup: Effect<u8> = sleep(Duration::from_secs(60)).map(|()| 1);
# assert_exit_success(&run_test(lookup.timeout(Duration::from_secs(5))), &None);
# let eff = Effect::retry(|| fetch_flaky(), Schedule::spaced(Duration::from_secs(1)).intersect(Schedule::recurs(3)));
# assert_exit_fail(&run_test(eff), &FetchError::Unavailable);
```

`run_test_on(&exec, eff)` runs on an executor you keep, so several effects share one virtual
clock (a cache TTL, a rate limiter window).

Every assertion is `#[track_caller]` and prints the whole exit on a mismatch.
`assert_exit_fail` and `assert_exit_die` look inside composite causes (`Both`, `Then`);
`assert_exit_kind` classifies a cause as `Die` if any leaf died, else `Failure` if any leaf
failed, else `Interrupt`.

## Golden files

```rust,no_run
use id_effect::{assert_golden, assert_golden_exit, run_test};
# use id_effect::{Effect, succeed};
# struct Report;
# fn sample() -> Report { Report }
# fn render_report(_report: &Report) -> String { "total: 0".to_owned() }
# struct Cart;
# fn empty_cart() -> Cart { Cart }
# fn checkout(_cart: Cart) -> Effect<u32, String> { succeed(0) }

#[test]
fn report_layout() {
  assert_golden("tests/golden/report.txt", &render_report(&sample()));
  assert_golden_exit("tests/golden/checkout_exit.txt", &run_test(checkout(empty_cart())));
}
# assert_golden("tests/golden/report.txt", &render_report(&sample()));
# assert_golden_exit("tests/golden/checkout_exit.txt", &run_test(checkout(empty_cart())));
```

- A relative path resolves against `CARGO_MANIFEST_DIR`; line endings are normalized.
- `ID_EFFECT_BLESS=1 cargo test` writes the current output instead of comparing. Review the
  diff and commit it. A missing file fails with that hint; a mismatch prints a line diff.
- `render_exit` drops what changes between runs (fiber ids), so an interrupted exit renders as
  `Failure(Interrupt)` every time.

## Effect-aware properties

`prop_check` runs an `Effect<A, TestCaseError>` like `run_test` and returns `Result<A,
TestCaseError>`, so it composes with `?` inside `proptest!`. `prop_sync` turns a closure that uses
`prop_assert!` into an effect, so the check can sit inside the pipeline. `prop_run` does the same
for any error type (a typed failure fails the case).

```rust
# // Doctests build without `--cfg test`, which drops `#[test]` items: run each property directly.
# macro_rules! proptest {
#   ($(#[test] fn $name:ident $args:tt $body:block)*) => {
#     ::proptest::proptest! { $(fn $name $args $body)* }
#     $($name();)*
#   };
# }
use id_effect::testing::property::{arb_duration, prop_check, prop_sync};
use id_effect::{Effect, sleep};
use proptest::prelude::*;
# /// Waits `gap`, then emits the last value.
# fn debounce<E: Send + 'static>(values: Vec<u8>, gap: std::time::Duration) -> Effect<u8, E> {
#   let last = *values.last().unwrap();
#   sleep(gap).map(move |()| last)
# }

proptest! {
  #[test]
  fn debounce_emits_last_value(values in prop::collection::vec(any::<u8>(), 1..20), gap in arb_duration()) {
    let last = *values.last().unwrap();
    let eff: Effect<u8, TestCaseError> = debounce(values, gap);
    let got = prop_check(eff.flat_map(move |v| prop_sync(move || {
      prop_assert_eq!(v, last);
      Ok(v)
    })))?;
    prop_assert_eq!(got, last);
  }
}
```

`prop_assert_exit_success`, `prop_assert_exit_fail` and `prop_assert_exit_eq` are the
`Result`-returning forms of the exit assertions.

## Generators from schemas

A `Schema` is a decoder, so `from_schema(schema, wire)` draws wire values and keeps those the
schema decodes. Every generated value passes the schema's refinements and brands, and a failing
case shrinks on the wire value. `arb_schema::<T>()` does it for a `HasSchema` type whose wire type
implements `Arbitrary`. `prop_assert_roundtrip` checks `decode(encode(a)) == a`.

```rust
# // Doctests build without `--cfg test`, which drops `#[test]` items: run each property directly.
# macro_rules! proptest {
#   ($(#[test] fn $name:ident $args:tt $body:block)*) => {
#     ::proptest::proptest! { $(fn $name $args $body)* }
#     $($name();)*
#   };
# }
use id_effect::schema::{HasSchema, Unknown, i64, refine};
use id_effect::testing::property::{from_schema, prop_assert_roundtrip, unknown_object};
use id_effect::Schema;
use proptest::prelude::*;

#[derive(Clone, Debug, PartialEq, Schema)]
struct User {
  #[schema("user_name")]
  name: String,
  age: i64,
}

fn user_wire() -> impl Strategy<Value = Unknown> {
  unknown_object([
    ("user_name", "[a-z]{1,8}".prop_map(Unknown::String).boxed()),
    ("age", (0..130i64).prop_map(Unknown::I64).boxed()),
  ])
}

proptest! {
  #[test]
  fn users_roundtrip(user in from_schema(User::schema(), user_wire())) {
    prop_assert_roundtrip(&<User as HasSchema>::schema(), user)?;
  }

  #[test]
  fn even_numbers(n in from_schema(refine(i64::<()>(), |n| n % 2 == 0, "odd"), -500i64..500)) {
    prop_assert_eq!(n % 2, 0);
  }
}
```

Pick a wire strategy that mostly decodes: rejected wire values count as local rejects, and
proptest gives up after too many. A `#[derive(Schema)]` struct has a JSON (`Unknown`) wire, and
arbitrary `Unknown` trees almost never decode to it, so build its wire field by field with
`unknown_object` (wire names, not Rust names). `arb_unknown()` generates arbitrary JSON-shaped
trees, for decoders that must reject garbage without panicking.

## Generators for framework types

| Strategy | Values |
|---|---|
| `arb_duration()`, `arb_duration_up_to(max)` | `Duration`s across scales (zero, µs, ms, s, up to a day) / uniform up to `max` |
| `arb_chunk(element, size)` | `Chunk<A>` |
| `arb_fiber_id()`, `arb_defect()` | `FiberId`, `Defect` |
| `arb_cause(error)` | `Cause<E>` trees of `Fail` / `Die` / `Interrupt` under `Both` / `Then`, depth ≤ 3 |
| `arb_exit(value, error)`, `arb_exit_success(value)`, `arb_exit_fail(error)` | `Exit<A, E>` |
| `arb_hcons(head, tail)`, `arb_hnil()` | capability lists `HCons<H, T>` for `provide_env` |

They shrink like their proptest building blocks: durations towards zero, chunks by dropping
elements, causes towards a single leaf.

## Stateful (model-based) testing

Implement `StateMachine` for a pure model of the system, and `run_state_machine` generates
command lists, runs each against the model and against the real system, and shrinks a failure to
a minimal list of commands.

```rust
use std::collections::VecDeque;

use id_effect::testing::property::{StateMachine, prop_sync, run_state_machine};
use id_effect::{Effect, Queue, succeed};
use proptest::prelude::*;
use proptest::test_runner::Config;

#[derive(Clone, Debug)]
enum Cmd { Offer(u8), Poll }

struct BoundedQueue { cap: usize }

impl StateMachine for BoundedQueue {
  type State = VecDeque<u8>;
  type Sut = Queue<u8>;
  type Command = Cmd;

  fn initial_state(&self) -> VecDeque<u8> { VecDeque::new() }

  fn commands(&self) -> BoxedStrategy<Cmd> {
    prop_oneof![any::<u8>().prop_map(Cmd::Offer), Just(Cmd::Poll)].boxed()
  }

  // A full queue would suspend `offer`: skip the command instead.
  fn precondition(&self, model: &VecDeque<u8>, cmd: &Cmd) -> bool {
    !matches!(cmd, Cmd::Offer(_)) || model.len() < self.cap
  }

  fn next_state(&self, model: &VecDeque<u8>, cmd: &Cmd) -> VecDeque<u8> {
    let mut m = model.clone();
    match cmd { Cmd::Offer(n) => m.push_back(*n), Cmd::Poll => { m.pop_front(); } }
    m
  }

  fn init_sut(&self) -> Effect<Queue<u8>, TestCaseError> { succeed(Queue::bounded(self.cap)) }

  fn apply(&self, q: &Queue<u8>, cmd: &Cmd, before: &VecDeque<u8>, _after: &VecDeque<u8>)
    -> Effect<(), TestCaseError>
  {
    match cmd {
      Cmd::Offer(n) => q.offer(*n).map(|_| ()),
      Cmd::Poll => {
        let front = before.front().copied();
        q.poll()
          .map_err(|e| TestCaseError::fail(format!("{e:?}")))
          .flat_map(move |got| prop_sync(move || { prop_assert_eq!(got, front); Ok(()) }))
      }
    }
  }
}

#[test]
fn bounded_queue_matches_its_model() {
  run_state_machine(&BoundedQueue { cap: 3 }, Config::with_cases(64), 0..40);
}
# run_state_machine(&BoundedQueue { cap: 3 }, Config::with_cases(64), 0..40);
```

- A command whose `precondition` fails in the current model state is skipped, so commands can be
  drawn independently of the state and shrinking simply drops and simplifies them.
- All effects of one case run on one `TestExecutor`: a command that sleeps advances the virtual
  clock for the commands after it.
- A failure panics with the step, the command, the model state and the minimal command list.
  Failure seeds are persisted next to the calling test file, as with `proptest!`.
- Inside your own `proptest!`, use `commands(&machine, size)` as the strategy and
  `check_commands(&machine, &cmds)?` as the body.

The runnable version of all of this is
`cargo run -p id_effect --features proptest --example 110_property_testing`.
