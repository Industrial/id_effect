# Your First Real Program

Let's build something complete: a small program that loads configuration, looks up a user and
formats a greeting. It fits on one page but shows the whole workflow: typed errors, composition with
`effect!`, a capability in `R`, and one run edge.

## The Domain

```rust
#[derive(Debug, Clone)]
struct Config {
  app_name: String,
}

#[derive(Debug, Clone)]
struct User {
  id: u64,
  name: String,
  email: String,
}

#[derive(Debug, PartialEq)]
enum AppError {
  Config(String),
  NotFound(u64),
}
```

The user store is a capability: a plain value the program reads from its environment.

```rust
use std::collections::HashMap;

struct UserStore(HashMap<u64, User>);
```

## The Individual Steps

```rust
use id_effect::{Effect, caps, effect, fail, succeed};

fn load_config(raw_name: &str) -> Effect<Config, AppError> {
  if raw_name.is_empty() {
    fail(AppError::Config("app name is empty".into()))
  } else {
    succeed(Config { app_name: raw_name.to_string() })
  }
}

fn fetch_user(id: u64) -> Effect<User, AppError, caps!(UserStore)> {
  effect!({ ~UserStore.0.get(&id).cloned().ok_or(AppError::NotFound(id))? })
}

fn format_greeting(config: &Config, user: &User) -> String {
  format!("{}: Hello, {}! ({})", config.app_name, user.name, user.email)
}
```

`~UserStore` reads the capability; `?` on the `Option`-turned-`Result` fails the effect with
`AppError::NotFound`.

## Composing the Program

```rust
fn greet_user(app_name: &str, user_id: u64) -> Effect<String, AppError, caps!(UserStore)> {
  let config = load_config(app_name);
  effect!({
    let config = ~config;
    let user = ~fetch_user(user_id);
    format_greeting(&config, &user)
  })
}
```

Read it like a recipe:

1. load the config, and stop with `AppError::Config` if that fails;
2. fetch the user, and stop with `AppError::NotFound` if there is none;
3. format the greeting, which always succeeds.

`load_config` needs nothing, `fetch_user` needs a `UserStore`; `~` lifts each into the block's
`caps!(UserStore)`. Nothing has run yet: `greet_user("Greeter", 42)` is a value.

## Running It

At the edge of the program, provide the capability and run:

```rust
use id_effect::{Cause, Exit};

fn main() {
  let store = UserStore(HashMap::from([(
    42,
    User { id: 42, name: "Alice".into(), email: "alice@example.com".into() },
  )]));

  match greet_user("Greeter", 42).provide(store).block_on() {
    Exit::Success(greeting) => println!("{greeting}"),
    Exit::Failure(Cause::Fail(AppError::Config(msg))) => eprintln!("config error: {msg}"),
    Exit::Failure(Cause::Fail(AppError::NotFound(id))) => eprintln!("no user {id}"),
    Exit::Failure(other) => eprintln!("unexpected: {other:?}"),
  }
}
```

`.provide(store)` removes `UserStore` from `R`; an `Effect<_, _, HNil>` can be run with
`block_on`. Forgetting the `.provide` is a compile error, not a runtime panic.

## Testing It

A test provides its own store:

```rust
#[test]
fn unknown_user_is_not_found() {
  let exit = greet_user("TestApp", 7)
    .provide(UserStore(HashMap::new()))
    .block_on();
  assert_eq!(exit, Exit::Failure(Cause::Fail(AppError::NotFound(7))));
}
```

No mocking framework and no `Arc<dyn Trait>` plumbing: the test supplies a different value for the
capability.

## What You Just Learned

- `succeed` and `fail` construct effects from values;
- `effect!({ .. })` with `~` sequences effects and reads capabilities;
- `caps!(..)` declares what an effect needs, and `.provide(..)` supplies it;
- `block_on` runs a fully provided effect at the program edge and returns an `Exit`.

Chapter 3 explores `effect!` in detail, and Chapter 4 begins the tour of `R`, the environment type
that makes dependency injection a compile-time guarantee.
