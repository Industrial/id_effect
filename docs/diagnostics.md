# Diagnostics gallery

The 20 most likely mistakes in id_effect application code. For each one: what you wrote, the error
you get and the fix. Every case is a compile-fail test in
[`crates/id_effect/tests/ui/gallery/`](../crates/id_effect/tests/ui/gallery). The `.stderr` next to
each `.rs` file holds the full blessed output, from the rustc version pinned as `BLESSED_RUSTC` in
`crates/id_effect/tests/ui.rs` (the same version CI pins).

**Fix in the error** (KPI K4) says whether the first error already states the fix. It does when
one of our `#[diagnostic::on_unimplemented]` notes, a proc-macro message, or rustc's own help or
note names the change to make. A "no" means rustc gives the cause but not the fix, and nothing in
our crates can add one through `on_unimplemented`. The fix column below is the reference for those
cases.

K4: 17 of the 20 cases state their fix. The other three come from rustc checks that no
id_effect trait takes part in:

- `not_send_body` (#14): the `Send` auto trait on the body's future. rustc names the `!Send` value
  and the await point, not the fix.
- `generic_runner_unannotated` (#15): E0283 is an inference ambiguity, not an unimplemented trait,
  so no `on_unimplemented` note applies. rustc's turbofish suggestion is wrong.
- `block_on_in_async` (#20): the std `Future` trait on `Exit`. rustc suggests removing `.await`,
  which compiles but blocks a thread inside async code.

## Capabilities

| # | Case | Mistake | First error | Fix | Fix in the error |
|---|------|---------|-------------|-----|------------------|
| 1 | `missing_capability` | `~Mailer` in a body whose `caps!(..)` has no `Mailer` | E0277: capability `Mailer` is not in this effect's environment | Add `Mailer` to the effect's `caps!(..)` | yes (`Selector` note) |
| 2 | `helper_needs_more_caps` | `~count_users()` where the helper needs `Db` and the caller does not have it | E0277: the environment `HCons<Logger, HNil>` does not contain every capability of `HCons<Db, HNil>` | Add `Db` to the caller's `caps!(..)` | yes (`Embed` note) |
| 3 | `duplicate_capability` | `caps!(Config, Db, Config)` | capability `Config` appears twice in `caps!(..)`; remove the repeated `Config` | Remove the second `Config` | yes (`caps!` message) |

## Providing

| # | Case | Mistake | First error | Fix | Fix in the error |
|---|------|---------|-------------|-----|------------------|
| 4 | `provide_not_required` | `.provide(Cache)` on an effect that never reads `Cache` | E0277: `Cache` is not required by this effect, so it cannot be provided | Drop the `.provide(..)` | yes (`Plucker` note) |
| 5 | `provide_twice` | `.provide(Db).provide(Db)` | E0277: `Db` is not required by this effect, so it cannot be provided | Drop the second `.provide(Db)` | yes (`Plucker` note) |
| 6 | `provide_some_extra` | `.provide_some(hlist![Db, Cache])` where `Cache` is not needed | E0277: `HCons<Cache, HNil>` is not a part of this effect's environment | Drop `Cache` from the list | yes (`Sculptor` note) |
| 7 | `provide_env_incomplete` | `.provide_env(hlist![Db])` for an effect over `caps!(Db, Logger)` | E0277: the environment `HCons<Db, HNil>` does not contain every capability of `HCons<Db, HCons<Logger, HNil>>` | Pass the whole environment, `hlist![Db, Logger]` | yes (`Embed` note) |

## Layers

| # | Case | Mistake | First error | Fix | Fix in the error |
|---|------|---------|-------------|-----|------------------|
| 8 | `layer_output_missing` | `.provide_layer(db_layer())` for an effect that also needs `Mailer` | E0277: the environment `HCons<Db, HNil>` does not contain every capability of `HCons<Db, HCons<Mailer, HNil>>` | Add a layer for `Mailer`, `db_layer().merge(mailer_layer())` | yes (`Embed` note) |
| 9 | `layer_provide_to_missing` | `db_layer().provide_to(repo_layer())` where the repo layer also needs `Config` | E0277: the environment `HCons<Db, HNil>` does not contain every capability of `HCons<Db, HCons<Config, HNil>>` | Give the base layer `Config`, `db_layer().merge(config_layer())` | yes (`Embed` note) |
| 10 | `provider_missing_provides` | `#[derive(ProviderSpecDerive)]` without `#[provides(..)]` | derive(ProviderSpec) requires #[provides(Service)] on the struct | Add `#[provides(Counter, with = \|\| Counter(0))]` | yes (derive message) |

## `effect!` bodies

| # | Case | Mistake | First error | Fix | Fix in the error |
|---|------|---------|-------------|-----|------------------|
| 11 | `bind_result` | `~parse("42")` on a function that returns a `Result` | E0277: `Result<u32, ParseError>` cannot be bound with `~` | `parse("42")?` | yes (`Bindable` note) |
| 12 | `bind_future` | `~fetch()` on an `async fn` | E0277: `impl Future<Output = u32>` cannot be bound with `~` | `fetch().await` | yes (`Bindable` note) |
| 13 | `join_in_effect_local` | `~(a, b)` in `effect_local!` | a parallel join `~(a, b, ..)` needs `effect!` | Use `effect!`, or bind the effects one at a time | yes (`effect_local!` message) |
| 14 | `not_send_body` | An `Rc` held across a `~ … .await` in `effect!` | future cannot be sent between threads safely | Use `Arc`, drop the value before the suspension point, or use `effect_local!` | no (rustc names the `!Send` value and the await, not the fix) |
| 15 | `generic_runner_unannotated` | `execute(env, \|\| effect!({ ~Counter.0 }))` with a runner generic over a subset `R` | E0283: type annotations needed | Annotate the closure, `\|\| -> Effect<_, _, caps!(Counter)> { .. }` | no (rustc suggests a turbofish on `Counter`, which is wrong) |

## Errors

| # | Case | Mistake | First error | Fix | Fix in the error |
|---|------|---------|-------------|-----|------------------|
| 16 | `error_not_convertible` | `~load()` with error `DbError` in a body whose error is `AppError` | E0277: the error `DbError` of this `~` bind does not widen into the body's error `AppError` | `impl From<DbError> for AppError`, or `~load().map_err(..)` | yes (`WidenFrom` note) |
| 17 | `never_error_mismatch` | `~load()` (fallible) inside an `Effect<_, Never>` | E0277: the error `DbError` of this `~` bind does not widen into the body's error `Infallible` | Handle it first, `~load().catch(..)`, or give the body a real error type | yes (`WidenFrom` note) |
| 18 | `or_error_right_arm` | `~send()` with error `MailError` in a body over `Or<DbError, MailError>` | E0277: the error `MailError` of this `~` bind does not widen into the body's error `Or<DbError, MailError>` | `~send().map_err(Or::Right)` (`From` only lifts into `Or::Left`) | yes (`WidenFrom` note) |

## Running

| # | Case | Mistake | First error | Fix | Fix in the error |
|---|------|---------|-------------|-----|------------------|
| 19 | `retry_without_factory` | `Effect::retry(fetch(), schedule)` | E0277: expected an `Fn()` closure, found `Effect<String, HttpError>` | `Effect::retry(fetch, schedule)` or `Effect::retry(\|\| fetch(), schedule)` | yes (rustc note: wrap it in a closure) |
| 20 | `block_on_in_async` | `job().block_on().await` in an `async fn` | E0277: `Exit<u32, Infallible>` is not a future | `Runtime::global().run(job()).await` | no (rustc suggests removing `.await`, which compiles but blocks inside async code) |
