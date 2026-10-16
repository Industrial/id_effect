# R Revisited — More Than Just a Type Parameter

```rust
# use id_effect::{Effect, caps, need};
# struct User; struct DbError; struct Database;
fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)>
# { need::<Database>().map(|_| User) }
```

It reads "this needs a `Database`". What does that mean precisely?

## R as a contract

`R` is a promise the compiler holds you to. You cannot run `get_user(1)` until a `Database` has
been provided:

```rust
# use id_effect::{Effect, caps, need};
# struct User; struct DbError;
# struct Database;
# impl Database { fn connect(_url: &str) -> Self { Database } }
# fn get_user(_id: u64) -> Effect<User, DbError, caps!(Database)> { need::<Database>().map(|_| User) }
// Does not compile: `block_on` needs `Effect<_, _, HNil>`, this is `caps!(Database)`
// get_user(1).block_on();

// Compiles: `provide` removes `Database` from `R`
let exit = get_user(1).provide(Database::connect("mem://")).block_on();
```

Reading a capability that is not in `R` fails at compile time too, with a message that names it:

```text
error[E0277]: capability `Metrics` is not in this effect's environment
   = note: add `Metrics` to the effect's `caps!(..)`, or remove it with `.provide(..)` / `.provide_env(..)` before running
```

## R flows through composition

Inside `effect!`, `~sub_effect` accepts any sub-effect whose `R` is **contained in** the block's
`R` (in any order). The block declares the union:

```rust
# use id_effect::{Effect, caps, effect, need};
# struct User { id: u64 }
# struct Post; struct DbError; struct Database;
fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> {
  // ...
#   need::<Database>().map(move |_| User { id })
}
fn get_posts(user_id: u64) -> Effect<Vec<Post>, DbError, caps!(Database)> {
  // ...
#   need::<Database>().map(|_| vec![])
}

// Both need the same capability: still caps!(Database)
fn get_user_with_posts(id: u64) -> Effect<(User, Vec<Post>), DbError, caps!(Database)> {
  effect!({
    let user = ~get_user(id);
    let posts = ~get_posts(user.id);
    (user, posts)
  })
}
```

When the steps need different capabilities, the block needs both:

```rust
# use id_effect::{Effect, caps, effect, need};
# struct User; struct DbError; struct AuditError; struct Database; struct AuditLog;
# enum AppError { Db(DbError), Audit(AuditError) }
# impl From<DbError> for AppError { fn from(e: DbError) -> Self { AppError::Db(e) } }
# impl From<AuditError> for AppError { fn from(e: AuditError) -> Self { AppError::Audit(e) } }
# fn get_user(_id: u64) -> Effect<User, DbError, caps!(Database)> { need::<Database>().map(|_| User) }
fn audit(msg: String) -> Effect<(), AuditError, caps!(AuditLog)> {
  // ...
#   need::<AuditLog>().map(|_| ())
}

// AppError: From<AuditError> + From<DbError>
fn get_user_logged(id: u64) -> Effect<User, AppError, caps!(Database, AuditLog)> {
  effect!({
    ~audit(format!("fetching user {id}"));
    ~get_user(id)
  })
}
```

Forgetting `AuditLog` in the block's `caps!` is a compile error at the `~audit(..)`.

## Multiple requirements

```rust
# use id_effect::{Effect, caps, effect, need};
# struct Order { user_id: u64, total: u64 }
# struct User { email: String }
# struct Payment; struct Receipt;
# impl Receipt { fn new(_: Payment) -> Self { Receipt } }
# struct AppError;
# struct Database; struct PaymentGateway; struct Mailer; struct AuditLog;
# fn audit(_: String) -> Effect<(), AppError, caps!(AuditLog)> { need::<AuditLog>().map(|_| ()) }
# fn get_user(_: u64) -> Effect<User, AppError, caps!(Database)> {
#   need::<Database>().map(|_| User { email: "a@example.com".into() })
# }
# fn charge(_: u64) -> Effect<Payment, AppError, caps!(PaymentGateway)> { need::<PaymentGateway>().map(|_| Payment) }
# fn send_confirmation(_: &str) -> Effect<(), AppError, caps!(Mailer)> { need::<Mailer>().map(|_| ()) }
fn process_order(order: Order)
  -> Effect<Receipt, AppError, caps!(Database, PaymentGateway, Mailer, AuditLog)>
{
  effect!({
    ~audit("processing order".into());
    let user = ~get_user(order.user_id);
    let payment = ~charge(order.total);
    ~send_confirmation(&user.email);
    Receipt::new(payment)
  })
}
```

From the signature alone you know this function touches four capabilities.

## Why R instead of parameters?

```rust
# struct Order; struct Receipt; struct AppError;
# struct Database; struct PaymentGateway; struct Mailer; struct AuditLog;
fn process_order(
  order: Order,
  db: &Database,
  pay: &PaymentGateway,
  mail: &Mailer,
  log: &AuditLog,
) -> Result<Receipt, AppError> {
  // ...
#   Ok(Receipt)
}
```

Parameters work, but every layer of the call stack must accept and forward dependencies it may not
use. `R` carries the same information in the return type, and the edge supplies it once.

Two capabilities of the same Rust type cannot both be in one `R` (`caps!(Url, Url)` is a compile
error): the type is the identity, so wrap them in newtypes (`PrimaryDb(Pool)`, `ReplicaDb(Pool)`).

## The runtime side

At run time the environment is a value of the `R` type: an `HCons` of `Arc`ed capabilities.
Providing it is cheap (an `Arc` clone per capability), and reading one is a type-directed field
access resolved at compile time. Chapter 5 looks at this structure in detail.
