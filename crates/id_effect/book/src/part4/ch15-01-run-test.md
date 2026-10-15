# Running Effects in Tests

There is no special test harness. A test builds an effect, provides its capabilities, runs it at
the edge with `block_on`, and asserts on the result.

## Basic Usage

```rust,ignore
use id_effect::{Effect, Exit, succeed};

fn double(n: i32) -> Effect<i32> {
  succeed(n * 2)
}

#[test]
fn double_doubles() {
  assert_eq!(double(21).block_on(), Exit::succeed(42));
}
```

`Effect::block_on` runs the effect on the global runtime (`Runtime::global()`) and returns its
`Exit<A, E>`. It needs `R = HNil`, so everything the effect requires must be provided first.

## Asserting on Exit

`Exit` is `Success(A)` or `Failure(Cause<E>)`; a `Cause` is a typed failure (`Fail(e)`), a defect
(`Die`, e.g. a panic), an interruption (`Interrupt`), or a combination (`Both` / `Then`).

```rust,ignore
use id_effect::{Cause, Exit};

#[test]
fn parse_rejects_garbage() {
  assert_eq!(parse_port("http").block_on(), Exit::fail(PortError::NotANumber));
}

#[test]
fn panics_become_defects() {
  let exit = boom().block_on();
  assert!(matches!(exit, Exit::Failure(Cause::Die(_))));
}
```

`exit.into_result()` gives a `Result<A, Cause<E>>` for `?` or `expect`, and
`cause.fail_option()` extracts the typed error when there is one. Many test files define a small
helper that turns an `Exit` into a `Result<A, E>` and panics on defects:

```rust,ignore
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(other) => panic!("effect died or was interrupted: {}", other.pretty()),
  }
}
```

## Providing the Environment

An effect that needs capabilities can't be run until they are provided:

```rust,ignore
use id_effect::{Effect, Exit, Never, caps, effect, hlist};

#[derive(Debug)]
struct Greeting(&'static str);
#[derive(Debug)]
struct Counter(usize);

fn summary() -> Effect<String, Never, caps!(Greeting, Counter)> {
  effect!({ format!("{}:{}", ~Greeting.0, ~Counter.0) })
}

#[test]
fn summary_reads_both_capabilities() {
  let exit = summary().provide_env(hlist![Greeting("hi"), Counter(3)]).block_on();
  assert_eq!(exit, Exit::succeed("hi:3".to_string()));
}
```

`provide(value)` removes one capability from `R`; `provide_env(hlist![..])` supplies the whole
list; `provide_layer(layer)` builds it from a `Layer`. Leaving one out does not compile, so a test
can never silently run against a missing dependency.

## Async tests

`block_on` refuses to run inside an async context, where it would block an executor thread: it
runs nothing and returns a defect (`try_block_on` returns `Err(BlockOnInAsync)` instead). In a
`#[tokio::test]`, await the effect through a `Runtime`:

```rust,ignore
use id_effect::Runtime;

#[tokio::test]
async fn works_inside_tokio() {
  let exit = Runtime::global().run(double(21)).await;
  assert_eq!(exit, Exit::succeed(42));
}
```

## Interruption and leaks

Forked children are supervised by their parent: when a fiber ends, `block_on` interrupts and
awaits the root's children, so a test cannot leak a running fiber past its end. To test
interruption itself, fork the effect, interrupt the handle and assert on the `Exit`:

```rust,ignore
let exit = slow()
  .fork()
  .flat_map(|h| sleep(Duration::from_millis(20)).then(h.interrupt()))
  .block_on();
```

For anything time-based (sleeps, timeouts, retries), don't wait on the wall clock: run the effect
on a `TestExecutor` ([next section](./ch15-02-test-clock.md)).
