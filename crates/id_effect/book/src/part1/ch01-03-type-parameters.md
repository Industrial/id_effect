# The Three Type Parameters

Every `Effect` carries three type parameters, `Effect<A, E = Never, R = HNil>`. They answer the
three questions every computation must address:

- **A**: what do I produce when I succeed?
- **E**: what do I produce when I fail?
- **R**: what do I need in order to run?

## A: The Answer

`A` is the success type, the `T` of a `Result<T, E>`.

```rust
use id_effect::{Effect, succeed};
# struct User(&'static str);
# impl User { fn new(name: &'static str) -> Self { User(name) } }
# struct DbError;

let answer: Effect<i32, String> = succeed(42);
let user: Effect<User, DbError> = succeed(User::new("Alice"));
```

`.map()` changes `A` and keeps `E` and `R`:

```rust
# use id_effect::{Effect, succeed};
let numbers: Effect<i32, String> = succeed(21);
let doubled: Effect<i32, String> = numbers.map(|n| n * 2);
let text: Effect<String, String> = doubled.map(|n| n.to_string());
```

## E: The Error

`E` is the typed failure, the `E` of a `Result<T, E>`. It defaults to
[`Never`](../appendix-c-glossary.md): an `Effect<A>` cannot fail with a typed error.

```rust
use id_effect::{Effect, fail};

let failure: Effect<i32, String> = fail("something went wrong".to_string());
```

`.map_err()` changes `E`:

```rust
# use id_effect::{Effect, succeed};
# struct User; struct DbError;
# enum AppError { Database(DbError) }
# fn fetch_user(_: u64) -> Effect<User, DbError> { succeed(User) }
let db: Effect<User, DbError> = fetch_user(42);
let app: Effect<User, AppError> = db.map_err(AppError::Database);
```

Typed failures are not the only way an effect can end: a panic becomes a *defect* and an
interrupted fiber ends with an *interrupt*. All three are carried by `Cause<E>` inside
`Exit::Failure` ([Chapter 8](../part3/ch08-00-error-handling.md)).

## R: The Requirements

`R` is the **environment**: the capabilities this effect needs in order to run. It is a
type-level list (an HList) of capability types, spelled with `caps!`:

```rust
use id_effect::{Effect, Never, caps, effect, succeed};
# struct User { id: u64 }
# struct DbError;
# struct Database;
# impl Database { fn fetch_user(&self, id: u64) -> Result<User, DbError> { Ok(User { id }) } }
# struct AuditLog;
# impl AuditLog { fn record(&self, _line: String) {} }

// Self-contained: R is HNil (the default).
let standalone: Effect<i32, String> = succeed(42);

// Needs a `Database`.
fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> {
  effect!({ ~Database.fetch_user(id)? })
}

// Needs a `Database` and an `AuditLog`.
fn get_user_logged(id: u64) -> Effect<User, DbError, caps!(Database, AuditLog)> {
  effect!({
    let user = ~Database.fetch_user(id)?;
    ~AuditLog.record(format!("fetched {}", user.id));
    user
  })
}
```

Inside `effect!`, `~Database` reads the capability (an `&Arc<Database>`). Reading a capability
that is not in `R` is a compile error that names it:

```text
error[E0277]: capability `Metrics` is not in this effect's environment
   = note: add `Metrics` to the effect's `caps!(..)`, or remove it with `.provide(..)` / `.provide_env(..)` before running
```

**You cannot run an effect until its capabilities are provided.** `block_on` exists only for
`Effect<A, E, HNil>`, so you remove each requirement with `.provide(value)`, a whole environment
with `.provide_env(hlist![..])`, or a [`Layer`](../part2/ch06-00-layers.md) with
`.provide_layer(layer)`, usually once at the edge:

```rust
# use id_effect::{Effect, caps, effect};
# struct User { id: u64 }
# struct DbError;
# struct Database;
# impl Database {
#   fn connect() -> Self { Database }
#   fn fetch_user(&self, id: u64) -> Result<User, DbError> { Ok(User { id }) }
# }
# struct AuditLog;
# impl AuditLog { fn stdout() -> Self { AuditLog } fn record(&self, _line: String) {} }
# fn get_user_logged(id: u64) -> Effect<User, DbError, caps!(Database, AuditLog)> {
#   effect!({
#     let user = ~Database.fetch_user(id)?;
#     ~AuditLog.record(format!("fetched {}", user.id));
#     user
#   })
# }
use id_effect::hlist;

let exit = get_user_logged(42)
  .provide_env(hlist![Database::connect(), AuditLog::stdout()])
  .block_on();
# assert_eq!(exit.into_result().ok().map(|u| u.id), Some(42));
```

## Why R matters

```rust
# use id_effect::{Effect, caps, effect};
# struct Order; struct Receipt; struct OrderError;
# struct Database; struct PaymentGateway; struct Mailer; struct AuditLog;
fn process_order(order: Order)
  -> Effect<Receipt, OrderError, caps!(Database, PaymentGateway, Mailer, AuditLog)>
# { effect!({ let _ = (~Database, ~PaymentGateway, ~Mailer, ~AuditLog); Receipt }) }
```

From the type alone you know the success value, the error and which capabilities must be wired
before the program can run. The compiler checks the wiring.

## R flows through composition

A `~` bind inside `effect!` accepts any sub-effect whose `R` is part of the outer `R`, and widens
its error through `From`:

```rust
# use id_effect::{Effect, caps, effect, need};
# struct User { email: String }
# struct DbError; struct EmailError;
# enum AppError { Db(DbError), Email(EmailError) }
# impl From<DbError> for AppError { fn from(e: DbError) -> Self { AppError::Db(e) } }
# impl From<EmailError> for AppError { fn from(e: EmailError) -> Self { AppError::Email(e) } }
# struct Database; struct Mailer;
fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> {
  // ...
#   need::<Database>().map(|_| User { email: "a@example.com".into() })
}
fn send_email(to: &str, body: &str) -> Effect<(), EmailError, caps!(Mailer)> {
  // ...
#   need::<Mailer>().map(|_| ())
}

// AppError: From<DbError> + From<EmailError>
fn notify_user(id: u64) -> Effect<(), AppError, caps!(Database, Mailer)> {
  effect!({
    let user = ~get_user(id);
    ~send_email(&user.email, "Hello!");
  })
}
```

## Reading Effect Signatures

```rust
# use id_effect::{Effect, caps};
# struct ParseError; struct User; struct DbError; struct AppError;
# struct Database; struct Cache; struct AuditLog;
// Produces String, never fails, needs nothing
# type A =
Effect<String>
# ;

// Produces i32, can fail with ParseError, needs nothing
# type B =
Effect<i32, ParseError>
# ;

// Produces User, can fail with DbError, needs Database
# type C =
Effect<User, DbError, caps!(Database)>
# ;

// Produces (), can fail with AppError, needs three capabilities
# type D =
Effect<(), AppError, caps!(Database, Cache, AuditLog)>
# ;
```

## What's Next

Effects are descriptions, and `Effect<A, E, R>` encodes success, failure and requirements. Why is
it better to describe computations than to just do them? Laziness.
