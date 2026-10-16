# Stream vs Effect — When to Use Each

The choice is simple: how many values does your computation produce?

```text
Effect<A, E, R>  → produces exactly one A (or fails)
Stream<A, E, R>  → produces zero or more A values over time (or fails)
```

## Concrete Examples

```rust
# use id_effect::{Effect, Stream, caps};
# struct User;
# struct DbError;
# struct Db;
# trait UserRepo {
// Effect: get one user
fn get_user(id: u64) -> Effect<User, DbError, caps!(Db)>;

// Stream: all users, pulled page by page
fn all_users() -> Stream<User, DbError, caps!(Db)>;
# }
```

If you fetch 10 million rows into a `Vec` and return it as an `Effect`, you'll run out of memory.
A `Stream` pulls and processes them incrementally.

## Building Streams

```rust
use id_effect::{Exit, Stream, succeed};

Stream::<i64, ()>::range(1, 5);                     // 1, 2, 3, 4
Stream::<i32, ()>::from_iterable([1, 2, 3]);        // any finite IntoIterator
Stream::<i32, ()>::unfold(1, |n| (n <= 4).then_some((n, n + 1)));
Stream::<i32, ()>::from_effect(succeed(vec![1, 2, 3])); // one effect yielding a batch
Stream::<i32, ()>::succeed(7);                      // one element
Stream::<i32, &str>::fail("boom");                  // fails on the first pull
```

Constructors are generic over `E` and `R`; annotate one of them (`Stream::<i32, ()>::…`) when
nothing else fixes them. For incremental sources, `from_pull(|max| effect_of_option_vec)` runs an
effect per pull (`Ok(None)` ends the stream), `unfold_effect` threads state through an effect, and
`Stream::from_queue(queue)` drains a `Queue`.

## Transforming Streams

```rust
# use id_effect::{Exit, Stream};
let s = Stream::<i64, ()>::range(1, 10)
  .map(|n| n * 2)
  .filter(|n: &i64| *n % 3 == 0)
  .run_collect();
assert_eq!(s.block_on(), Exit::succeed(vec![6, 12, 18]));
```

The operators: `map` / `filter` (pure, `Fn`), `map_serial` / `filter_serial` (`FnMut`, in order),
`take`, `take_while`, `drop_while`, `scan`, `grouped` / `tumbling` / `sliding`, `flat_map`,
`chain` / `concat`, `merge`, `map_err`, `widen`, and the effectful `map_effect` (sequential) /
`map_effect_par(n, f)` (at most `n` in flight, output in stream order). None of them load the
whole stream into memory.

`take(n)` on an infinite source stops pulling after `n` elements, so the source never runs ahead
of the consumer:

```rust
# use id_effect::{Exit, Stream};
let first = Stream::<u64, ()>::unfold(0, |n| Some((n, n + 1))).take(3).run_collect();
assert_eq!(first.block_on(), Exit::succeed(vec![0, 1, 2]));
```

## Consuming a Stream

A consumer turns `Stream<A, E, R>` into `Effect<B, E, R>`:

```rust
# use id_effect::{Effect, Exit, Stream, caps};
# #[derive(Debug)]
# struct User;
# #[derive(Debug)]
# struct DbError;
# struct Db;
# fn all_users() -> Stream<User, DbError, caps!(Db)> { Stream::from_iterable([User, User]) }
let all: Effect<Vec<User>, DbError, caps!(Db)> = all_users().run_collect();
let count: Effect<usize, DbError, caps!(Db)> = all_users().run_fold(0, |n, _| n + 1);
# assert_eq!(all.provide(Db).block_on().into_result().map(|users| users.len()).ok(), Some(2));
# assert!(matches!(count.provide(Db).block_on(), Exit::Success(2)));
```

`run_collect` accumulates into a `Vec`; use it only when the result fits in memory. Prefer
`run_fold`, `run_for_each(|a| effect)`, `run_drain` or a `Sink` for large streams.

## Effect to Stream and back

```rust
# use id_effect::{Effect, Exit, Stream, caps, succeed};
# #[derive(Debug)]
# struct User;
# #[derive(Debug)]
# struct DbError;
# struct Db;
# fn get_user(_id: u64) -> Effect<User, DbError, caps!(Db)> { succeed(User) }
# fn all_users() -> Stream<User, DbError, caps!(Db)> { Stream::from_iterable([User, User]) }
let one: Stream<User, DbError, caps!(Db)> =
  Stream::from_effect(get_user(1).map(|u| vec![u]));
let combined = one.chain(all_users());
# assert!(matches!(combined.run_fold(0, |n, _| n + 1).provide(Db).block_on(), Exit::Success(3)));
```

## The Rule

- Need one result: `Effect`
- Need to process many results without loading them all at once: `Stream`
- Need to compose sources: `Stream` with `chain`, `merge`, `combine_latest`, `keyed_join`
- Need all results in memory: `Stream` + `run_collect()` (mind the size)
