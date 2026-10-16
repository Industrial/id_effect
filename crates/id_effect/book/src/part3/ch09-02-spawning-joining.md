# Spawning and Joining — zip_par, all, race and Friends

Forking a single fiber is useful; running many concurrently is where fibers shine. You rarely need `fork` + `join` by hand: the parallel combinators do it for you, with structured semantics (children are child fibers of the caller, the first failure interrupts the siblings, and nothing returns before every started child has finished).

## fork: Spawn One Fiber

```rust
# use id_effect::{Effect, Exit, caps, succeed};
# #[derive(Debug, PartialEq, Eq, Hash)]
# struct Local(u8);
# #[derive(Debug, PartialEq, Eq, Hash)]
# struct Remote(u8);
# #[derive(Debug, PartialEq, Eq, Hash)]
# enum AppError {}
# struct Api;
# fn remote_call() -> Effect<Remote, AppError, caps!(Api)> { succeed(Remote(2)) }
# fn local_computation() -> Effect<Local, AppError, caps!(Api)> { succeed(Local(1)) }
let program: Effect<(Local, Remote), AppError, caps!(Api)> = remote_call()
    .fork()
    .widen()
    .flat_map(|handle| {
        local_computation().flat_map(move |local| handle.join().map(move |remote| (local, remote)))
    });
# assert_eq!(program.provide(Api).block_on(), Exit::succeed((Local(1), Remote(2))));
```

`fork` starts the effect on a child fiber; you do other work and `join` later.

## zip_par: Two Effects in Parallel

```rust
use std::time::Duration;
use id_effect::{Effect, Exit, Never, sleep, succeed, zip_par};

let pair: Effect<(u8, &str), Never> = zip_par(
    sleep(Duration::from_millis(5)).then(succeed(1)),
    succeed("two"),
);
assert!(matches!(pair.block_on(), Exit::Success((1, "two"))));
```

`zip_with_par(a, b, f)` combines the two values with `f`. Inside `effect!`, `~(a, b)` joins two sub-effects in parallel the same way.

## all and for_each_par: Run Many, Collect All

```rust
use id_effect::{Concurrency, Effect, Exit, Never, all, for_each_par, succeed};

let work: Vec<Effect<i32, ()>> = vec![succeed(10), succeed(20)];
assert_eq!(all(work, Concurrency::Unbounded).block_on(), Exit::succeed(vec![10, 20]));

let squares = for_each_par(1..=4_u32, |n| succeed::<_, Never, _>(n * n), Concurrency::Bounded(2));
assert!(matches!(squares.block_on(), Exit::Success(v) if v == [1, 4, 9, 16]));
```

Both keep input order. The `Concurrency` argument picks the scheduling:

| `Concurrency` | Behaviour |
|---|---|
| `Sequential` | a left fold on the current fiber; nothing is forked |
| `Bounded(n)` | at most `n` children in flight (a sliding window) |
| `Unbounded` | every child at once |
| `Inherit` | each child awaits a permit from the ambient [`Fabric`](./ch12-00-compute-fabric.md); `Unbounded` when there is none |

`fork_all(effects, concurrency)` is the primitive underneath all of them.

## race, race_all, race_first: First One Wins

```rust
# use id_effect::{Effect, Exit, fail, race_all, succeed};
# fn fetch_from_primary() -> Effect<&'static str, String> { fail("primary down".to_owned()) }
# fn fetch_from_backup() -> Effect<&'static str, String> { succeed("backup") }
# fn try_us() -> Effect<&'static str, String> { succeed("us") }
# fn try_eu() -> Effect<&'static str, String> { succeed("eu") }
# fn try_ap() -> Effect<&'static str, String> { succeed("ap") }
# let (a, b) = (fetch_from_primary(), fetch_from_backup());
let data = fetch_from_primary().race(fetch_from_backup()); // first *success* wins
let any = race_all([try_us(), try_eu(), try_ap()]);        // the same, for many
let first = a.race_first(b);                               // first *completion* wins
# assert_eq!(data.block_on(), Exit::succeed("backup"));
# assert!(any.block_on().into_result().is_ok());
# let _either_one: Exit<&str, String> = first.block_on(); // both are ready: either may complete first
```

The losers are interrupted and awaited, so their finalizers have run before the result is returned. `race` / `race_all` ignore failures until every racer has failed, then fail with all the causes combined (`Cause::Both`). `race_all` of no effects dies.

## timeout and timeout_fail

```rust
# use std::time::Duration;
# use id_effect::{Effect, Exit, caps, succeed};
# #[derive(Debug, PartialEq, Eq, Hash)]
# struct User;
# struct Db;
# #[derive(Debug, PartialEq, Eq, Hash)]
# enum DbError {}
# #[derive(Debug, PartialEq, Eq, Hash)]
# enum AppError { Db(DbError), Timeout }
# fn load_user(_id: u64) -> Effect<User, DbError, caps!(Db)> { succeed(User) }
# let id = 1;
let maybe: Effect<Option<User>, DbError, caps!(Db)> = load_user(id).timeout(Duration::from_secs(2));
let strict: Effect<User, AppError, caps!(Db)> =
    load_user(id).map_err(AppError::Db).timeout_fail(Duration::from_secs(2), || AppError::Timeout);
# assert_eq!(maybe.provide(Db).block_on(), Exit::succeed(Some(User)));
# assert_eq!(strict.provide(Db).block_on(), Exit::succeed(User));
```

`timeout` returns `None` when the deadline passes; `timeout_fail` fails with your error instead. The deadline is on the fiber's executor clock, so under `TestExecutor` it is virtual time ([Chapter 11](./ch11-04-clock-injection.md)).

## Error Behaviour

| Combinator | On failure |
|------------|------------|
| `zip_par`, `all`, `for_each_par`, `fork_all` | interrupt the in-flight siblings, wait for them, fail with the first failure (plus any genuine sibling failure as `Cause::Both`) |
| `race`, `race_all` | keep racing; fail with every cause only when all racers failed |
| `race_first`, `timeout` | the first completion decides, success or failure |
| `fork` + `join` | whatever the child's outcome is |

Choose based on whether partial success is acceptable and whether you want to wait for everyone. To collect *every* outcome instead, see [Error Accumulation](./ch08-04-accumulation.md).
