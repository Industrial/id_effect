# Composing Layers — merge, provide_to and provide_merge

Individual layers build one or a few capabilities. Three combinators wire them into a graph
(Effect.ts `Layer.merge`, `Layer.provide`, `Layer.provideMerge`).

## `merge`: side by side

`a.merge(b)` builds `a`, then `b`, and outputs both: its `Out` is `a`'s outputs followed by `b`'s,
its `In` everything either side needs.

```rust
use id_effect::{Effect, HCons, HNil, Layer, hlist, need};

struct Config(&'static str);
struct Db(&'static str);
struct Mailer(&'static str);

fn db() -> Layer<HCons<Db, HNil>, (), HCons<Config, HNil>> {
  Layer::effect(|| need::<Config>().map(|c| hlist![Db(c.0)]))
}
fn mailer() -> Layer<HCons<Mailer, HNil>, (), HCons<Config, HNil>> {
  Layer::effect(|| need::<Config>().map(|c| hlist![Mailer(c.0)]))
}

let services = db().merge(mailer()); // Out = caps!(Db, Mailer), In needs Config (twice)
```

A capability both sides need appears twice in the merged input; it is split back by position, so
it needs no annotation, and both sides receive the same `Arc`.

## `provide_merge`: feed and keep both

`upper.provide_merge(base)` builds `base`, feeds its output to `upper`, and keeps **both**
outputs (`base`'s first). The result needs only `base`'s input:

```rust
# use id_effect::{Effect, HCons, HNil, Layer, hlist, need};
# struct Config(&'static str);
# struct Db(&'static str);
# struct Mailer(&'static str);
# fn db() -> Layer<HCons<Db, HNil>, (), HCons<Config, HNil>> {
#   Layer::effect(|| need::<Config>().map(|c| hlist![Db(c.0)]))
# }
# fn mailer() -> Layer<HCons<Mailer, HNil>, (), HCons<Config, HNil>> {
#   Layer::effect(|| need::<Config>().map(|c| hlist![Mailer(c.0)]))
# }
let app = db()
  .merge(mailer())
  .provide_merge(Layer::succeed(hlist![Config("mem://")]));
let env: Effect<HCons<Config, HCons<Db, HCons<Mailer, HNil>>>, ()> = app.build();
```

## `provide_to`: feed and keep the upper output

`base.provide_to(upper)` feeds `base`'s output into `upper` and keeps only `upper`'s output, so
`base` becomes a private dependency:

```rust
# use id_effect::{HCons, HNil, Layer, hlist, need};
# struct Pool; struct Repo;
# fn pool() -> Layer<HCons<Pool, HNil>, ()> { Layer::succeed(hlist![Pool]) }
# fn repo() -> Layer<HCons<Repo, HNil>, (), HCons<Pool, HNil>> {
#   Layer::effect(|| need::<Pool>().map(|_| hlist![Repo]))
# }
// Only `Repo` is visible to the program; `Pool` stays inside the graph.
let repo_only = pool().provide_to(repo());
# let _: Layer<HCons<Repo, HNil>, ()> = repo_only;
```

## `provide_layer`: run an effect against a graph

```rust
# use id_effect::{Effect, Exit, HCons, HNil, Layer, hlist, need};
# struct Pool; struct Repo;
# fn pool() -> Layer<HCons<Pool, HNil>, ()> { Layer::succeed(hlist![Pool]) }
# fn repo() -> Layer<HCons<Repo, HNil>, (), HCons<Pool, HNil>> {
#   Layer::effect(|| need::<Pool>().map(|_| hlist![Repo]))
# }
# fn use_repo() -> Effect<(), (), HCons<Repo, HNil>> { need::<Repo>().map(|_| ()) }
let app: Effect<(), ()> = use_repo().provide_layer(pool().provide_to(repo()));
# assert_eq!(app.block_on(), Exit::succeed(()));
```

`provide_layer` builds the graph each time the effect runs, provides its output (which must contain
every capability of the effect's `R`, in any order, extras allowed) and closes the build's scope
afterwards. A missing capability is a compile error:

```text
error[E0277]: the environment `HCons<Db, HNil>` does not contain every capability of `HCons<Db, HCons<Mailer, HNil>>`
  |   let _ = handler().provide_layer(db_layer());
  |                     ^^^^^^^^^^^^^ cannot narrow `HCons<Db, HNil>` to `HCons<Db, HCons<Mailer, HNil>>`
```

## Memoisation

Within one build, a layer used in several places is constructed once:

```rust
# use std::sync::Arc;
# use id_effect::{HCons, HNil, Layer, hlist, need};
# struct Pool; struct Users(Arc<Pool>); struct Orders(Arc<Pool>);
# let pool: Layer<HCons<Pool, HNil>, ()> = Layer::succeed(hlist![Pool]);
# fn users() -> Layer<HCons<Users, HNil>, (), HCons<Pool, HNil>> {
#   Layer::effect(|| need::<Pool>().map(|p| hlist![Users(p)]))
# }
# fn orders() -> Layer<HCons<Orders, HNil>, (), HCons<Pool, HNil>> {
#   Layer::effect(|| need::<Pool>().map(|p| hlist![Orders(p)]))
# }
// `pool` is used twice (a clone is the same layer): built once, one shared `Arc<Pool>`.
let app = pool.clone().provide_to(users()).merge(pool.provide_to(orders()));
```

Two separate calls of a layer-returning function (`pool()` and `pool()`) are two layers and build
twice, as in Effect.ts. Bind the layer to a variable and clone it to share it. `.fresh()` forces a
layer to be constructed again even where it is already built.

## Finalizer order

The build's finalizers run in reverse order of acquisition when the owner ends:

```rust
# use std::sync::{Arc, Mutex};
# use id_effect::{Effect, Exit, HCons, HNil, Layer, hlist, need, sync};
# struct Pool; struct Repo;
# let log = Arc::new(Mutex::new(Vec::new()));
# let (l1, l2, l3) = (Arc::clone(&log), Arc::clone(&log), Arc::clone(&log));
# let pool: Layer<HCons<Pool, HNil>, ()> = Layer::acquire_release(
#   || sync(|| hlist![Pool]),
#   move |_, _| { let l = Arc::clone(&l1); sync(move || l.lock().unwrap().push("close pool")) },
# );
# let repo: Layer<HCons<Repo, HNil>, (), HCons<Pool, HNil>> = Layer::acquire_release(
#   || need::<Pool>().map(|_| hlist![Repo]),
#   move |_, _| { let l = Arc::clone(&l2); sync(move || l.lock().unwrap().push("close repo")) },
# );
# let use_repo = move || -> Effect<(), (), HCons<Repo, HNil>> {
#   need::<Repo>().flat_map(move |_| sync(move || l3.lock().unwrap().push("use")))
# };
let app: Effect<(), ()> = use_repo().provide_layer(pool.provide_to(repo));
assert!(matches!(app.block_on(), Exit::Success(())));
// log: ["use", "close repo", "close pool"]
# assert_eq!(*log.lock().unwrap(), ["use", "close repo", "close pool"]);
```

## Production vs test graphs

```rust
# use id_effect::{Effect, Layer, caps, effect, hlist, need};
# struct Config; struct Db; struct UserRepo; struct User;
# #[derive(Debug)] struct AppError;
# fn config_live() -> Layer<caps!(Config), AppError> { Layer::succeed(hlist![Config]) }
# fn db_live() -> Layer<caps!(Db), AppError, caps!(Config)> {
#   Layer::effect(|| need::<Config>().map(|_| hlist![Db]))
# }
# fn user_repo_live() -> Layer<caps!(UserRepo), AppError, caps!(Db)> {
#   Layer::effect(|| need::<Db>().map(|_| hlist![UserRepo]))
# }
# fn user_repo_mock() -> Layer<caps!(UserRepo), AppError> { Layer::succeed(hlist![UserRepo]) }
# fn get_user(_id: u64) -> Effect<User, AppError, caps!(UserRepo)> { effect!({ let _ = ~UserRepo; User }) }
fn live() -> Layer<caps!(Config, Db, UserRepo), AppError> {
  user_repo_live().provide_merge(db_live().provide_merge(config_live()))
}

fn test() -> Layer<caps!(UserRepo), AppError> {
  user_repo_mock()
}

let exit = get_user(1).provide_layer(live()).block_on();  // production
let exit = get_user(1).provide_layer(test()).block_on();  // test: only what get_user needs
```

Application code is unchanged; only the graph differs. A test graph must supply what the effect
under test needs, and nothing else, and the compiler checks that it does.

## Duplicate outputs

Two layers that both **produce** the same capability cannot be merged: `merge` and
`provide_merge` panic when the graph is wired ("capability `T` is produced by both sides of
Layer::merge; wrap one in a newtype"). Graphs are wired once at startup, so this surfaces before
any fiber runs.
