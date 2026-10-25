# Lifecycle hooks

Some work belongs to every write of an entity: normalise a field before it is stored, refuse a delete, record what changed, tell the rest of the system that an order was paid. Scattering that across every call site is how it gets forgotten. `id_effect_sql` (feature `repository`) lets you register **lifecycle hooks** on a repository: effects that run with every `insert`, `update` and `delete`, **inside the write's transaction**.

## Registering hooks

`EntityHooks<T, E>` collects the hooks of the entity `T`; `Repository::with_hooks` registers them and returns a `HookedRepository`:

| Hook | Receives | Returns |
|---|---|---|
| `before_insert`, `before_update` | the entity, by value | the entity to write (changed or not), or a failure that rejects the write |
| `after_insert`, `after_update` | the stored entity (generated id, new version) | `()`, or a failure that rolls the write back |
| `before_delete` | the primary key | `()`, or a failure that rejects the delete |
| `after_delete` | the primary key, only when a row was deleted | `()`, or a failure that rolls the delete back |

Every hook also receives a `HookContext`: the repository bound to the write's transaction (`ctx.repository()`), the transaction itself (`ctx.transaction()`), the dialect and the tenant. Several hooks for one event run in the order they were registered, each after the previous one finished.

```rust
use std::sync::{Arc, Mutex};

use id_effect::{Exit, succeed};
use id_effect_sql::repository::EntityHooks;
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, TestSqlClient, sql_client_env};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "customers")]
struct Customer {
    #[entity(id)]
    id: i64,
    email: String,
}

let seen = Arc::new(Mutex::new(Vec::new()));
let trace = Arc::clone(&seen);
let hooks = EntityHooks::<Customer>::new()
    .before_update(|_ctx, mut customer| {
        customer.email = customer.email.trim().to_lowercase();
        succeed(customer)
    })
    .after_update(move |ctx, customer| {
        trace.lock().unwrap().push(customer.email.clone());
        // A statement in the same transaction: it commits or rolls back with the update.
        ctx.transaction()
            .execute("INSERT INTO customer_changes (customer_id) VALUES ($1)", &[customer.id.into()])
            .map(|_| ())
    });
let customers = Repository::<Customer>::new(Dialect::Postgres).with_hooks(hooks);

let db = TestSqlClient::new();
let _ = db.connect().block_on();
db.script_execute(r#"UPDATE "customers" SET "email" = $1 WHERE "id" = $2"#, 1);
let updated = customers
    .update::<SqlClientEnv, _>(Customer { id: 1, email: "  Ada@Example.COM ".into() })
    .provide_all(sql_client_env(db.clone()))
    .block_on();
assert!(matches!(updated, Exit::Success(c) if c.email == "ada@example.com"));
assert_eq!(*seen.lock().unwrap(), ["ada@example.com"]);
// The UPDATE and the hook's INSERT, in one transaction that committed.
assert_eq!(db.recorded_executes().len(), 2);
assert_eq!((db.commits(), db.rollbacks()), (1, 0));
```

Hooks are **per repository**, never global: the plain `Repository` keeps writing without them (a data migration, an import), and two hooked repositories of one entity can carry different hooks. Reads go through `hooked.repository()`.

## One transaction, all or nothing

`HookedRepository::insert` / `update` / `delete` / `hard_delete` open a transaction on the `SqlClientService` in the environment and run the `before` hooks, the statement and the `after` hooks in it. If any of them fails, the transaction rolls back: the row is not written, and nothing a hook wrote through `ctx.transaction()` survives either.

```rust
use id_effect::{Exit, fail};
use id_effect_sql::repository::EntityHooks;
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlError, TestSqlClient, sql_client_env};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "customers")]
struct Customer {
    #[entity(id)]
    id: i64,
    email: String,
}

let hooks = EntityHooks::<Customer>::new()
    .after_update(|_ctx, _customer| fail(SqlError::Unsupported("downstream refused".into())));
let customers = Repository::<Customer>::new(Dialect::Postgres).with_hooks(hooks);

let db = TestSqlClient::new();
let _ = db.connect().block_on();
db.script_execute(r#"UPDATE "customers" SET "email" = $1 WHERE "id" = $2"#, 1);
let updated = customers
    .update::<SqlClientEnv, _>(Customer { id: 1, email: "ada@example.com".into() })
    .provide_all(sql_client_env(db.clone()))
    .block_on();
assert!(matches!(updated, Exit::Failure(_)));
// The UPDATE ran, then the hook failed: the transaction rolled back.
assert_eq!((db.commits(), db.rollbacks()), (0, 1));
```

Already inside a transaction? `hooked.in_transaction(&tx)` returns a `BoundHookedRepository` whose writes run their hooks and statement in a **savepoint** of the caller's transaction. A failing hook rolls back to the savepoint, so its write is undone even when the caller catches the error and commits the rest of its work; the caller commits.

## Rejecting a write with your own error type

The hooks of an `EntityHooks<T, E>` fail with `E`: any error type with `From<SqlError>` (the default is `SqlError`). A `before_*` hook can therefore reject a write with a domain error, and the hooked repository's writes fail with the same `E`, SQL failures converted into it.

```rust
use id_effect::{Cause, Exit, fail, succeed};
use id_effect_sql::repository::EntityHooks;
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlError, TestSqlClient, sql_client_env};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "invoices")]
struct Invoice {
    #[entity(id)]
    id: i64,
    status: String,
}

#[derive(Debug, PartialEq)]
enum InvoiceError {
    Sql(SqlError),
    Locked(i64),
}

impl From<SqlError> for InvoiceError {
    fn from(e: SqlError) -> Self {
        InvoiceError::Sql(e)
    }
}

let hooks = EntityHooks::<Invoice, InvoiceError>::new().before_delete(|_ctx, id| {
    if *id < 100 { fail(InvoiceError::Locked(*id)) } else { succeed(()) }
});
let invoices = Repository::<Invoice>::new(Dialect::Postgres).with_hooks(hooks);

let db = TestSqlClient::new();
let _ = db.connect().block_on();
let deleted = invoices
    .delete::<SqlClientEnv, _>(7)
    .provide_all(sql_client_env(db.clone()))
    .block_on();
assert!(matches!(deleted, Exit::Failure(Cause::Fail(InvoiceError::Locked(7)))));
assert!(db.recorded_executes().is_empty(), "the DELETE never ran");
```

## Secure by default

- **Hooks cannot leave the tenant.** `ctx.repository()` is the hooked repository bound to the write's transaction, with its tenant, soft-delete and custom filters: what a hook reads or writes through it stays in the tenant ([Audit and tenancy](ch30-07-audit-tenancy.md)). Lifting the filters takes the same explicit `Repository::unscoped()` as anywhere else.
- `ctx.transaction()` runs raw statements, which are **not** tenant-filtered; use it for other tables (an outbox row, an audit record), bind every value, and write the tenant yourself.
- A tenant-scoped entity under a repository without a tenant fails closed (`SqlError::Unsupported`) inside the transaction, before any statement runs.

## The audit log as a hook

The audit log (feature `audit`) keeps its explicit `AuditLog::append(&tx, ..)`: nothing is audited behind your back. To audit every write of an entity, call it from an `after_*` hook with the write's transaction, so the record commits or rolls back with the write:

```rust,no_run
use id_effect_sql::audit::{AuditContext, AuditEntry, AuditLog};
use id_effect_sql::repository::EntityHooks;
use id_effect_sql::{Dialect, Entity, FromRow, Repository};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "invoices", tenant = "org_id")]
struct Invoice {
    #[entity(id)]
    id: i64,
    status: String,
}

let (log, actor) = (AuditLog::new(), AuditContext::for_tenant("user:42", "acme"));
let hooks = EntityHooks::<Invoice>::new().after_update(move |ctx, invoice| {
    let entry = AuditEntry::new("invoice.update", format!("invoice:{}", invoice.id))
        .with_after(id_effect_sql::serde_json::json!({ "status": invoice.status }));
    log.append(ctx.transaction(), &actor, entry).map(|_| ())
});
let invoices = Repository::<Invoice>::new(Dialect::Postgres).with_tenant("acme").with_hooks(hooks);
# let _ = invoices;
```

## Events through the transactional outbox

"Update the order, then publish `orders.paid`" has two failure modes: the update commits and the publish is lost, or the publish goes out and the update rolls back. The transactional outbox ([Messaging](ch31-02-messaging.md)) closes both: the event is a row written in the same transaction, and a relay publishes the committed rows afterwards. An `after_*` hook is the natural place to write it.

With `id_effect_messaging`, stage the message with the hook's transaction:

```rust,no_run
use id_effect_messaging::{Message, Outbox, Topic};
use id_effect_sql::repository::EntityHooks;
use id_effect_sql::{Dialect, Entity, FromRow, Repository};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "orders")]
struct Order {
    #[entity(id)]
    id: i64,
    status: String,
}

let (outbox, paid) = (Outbox::new(), Topic::new("orders.paid").expect("topic"));
let hooks = EntityHooks::<Order>::new().after_update(move |ctx, order| {
    let message = Message::new(format!(r#"{{"id":{}}}"#, order.id)).with_key(order.id.to_string());
    outbox.stage(ctx.transaction(), &paid, message).map(|_| ())
});
let orders = Repository::<Order>::new(Dialect::Postgres).with_hooks(hooks);
# let _ = orders;
```

Feature `outbox` of `id_effect_sql` packages the same pattern as an adapter, `repository::outbox`: `EntityOutbox::new(sink)` builds `after_*` hooks from a function returning the event (`on_save` for inserts and updates, `on_delete` for deletes; `None` stages nothing). The sink `OutboxTable` inserts into `id_effect_outbox` in `id_effect_messaging`'s row format, in the statement dialect you choose, so the messaging `Relay` publishes the rows; any `Fn(&Arc<dyn SqlTransaction>, OutboxEvent) -> Effect<(), SqlError>` is a sink too. (`id_effect_messaging` depends on `id_effect_sql`, so the feature cannot depend back on it; the adapter writes the shared table format instead.)

```rust,ignore
use id_effect_sql::repository::outbox::{EntityOutbox, OutboxEvent, OutboxTable};

let outbox = EntityOutbox::new(OutboxTable::new(Dialect::Postgres));
let hooks = EntityHooks::<Order>::new().after_update(outbox.on_save(|order: &Order| {
    (order.status == "paid").then(|| {
        OutboxEvent::json("orders.paid", &serde_json::json!({ "id": order.id }))
            .with_key(order.id.to_string())
    })
}));
```

If staging fails (an invalid topic, the table missing) the hook fails and the update rolls back: there is never a write without its event, nor an event without its write.

## Tests

`crates/id_effect_sql/tests/hooks.rs` runs `hooks_run_in_order_inside_transaction`, `hook_failure_rolls_back_write`, `before_hook_can_reject_with_typed_error` and `after_update_writes_outbox_event_atomically` on SQLite in memory and on PostgreSQL in its own container:

- `cargo nextest run -p id_effect_sql --features repository,sqlite,outbox --test hooks`
- `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_sql --features repository,pg,outbox --test hooks`
