# Relations and eager loading

[Repositories](ch28-03-repositories.md) read one entity's table. Most screens need a row and the rows around it: an order with its lines and its customer, a post with its tags. Loading those one parent at a time is the N+1 problem: one query for the orders, then one more per order. `id_effect_sql` (feature `repository`) declares relations on the entity and loads them in **one query per relation per level**, whatever the number of rows.

## Declaring relations

Three attributes on `#[derive(Entity)]`, each naming the target entity first:

| Attribute | Kind | Children are the target rows whose … |
|---|---|---|
| `belongs_to(Customer, foreign_key = "customer_id")` | N:1 | primary key is this row's `customer_id` (a field) |
| `has_many(Line, foreign_key = "order_id")` | 1:N | `order_id` is this row's primary key |
| `many_to_many(Tag, through = "order_tags", local_key = "order_id", foreign_key = "tag_id")` | N:M | primary key is paired with this row's in the join table |

`name = ".."` names the relation. The defaults follow the usual conventions: the name is the target in snake case (`customer`), plus `s` for the two "many" kinds (`lines`, `tags`); `belongs_to`'s foreign key is `<target>_id`, `has_many`'s is `<this entity>_id`, and the join table's keys are `<this entity>_id` and `<target>_id`. A `belongs_to` foreign key must be a column of the entity (the derive checks it).

Each relation becomes a constructor, `Order::lines() -> Relation<Order, Line>`, and an entry of `Entity::RELATIONS`:

```rust
use id_effect_sql::repository::RelationKind;
use id_effect_sql::{Entity, FromRow};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "customers", tenant = "org_id")]
struct Customer {
    #[entity(id)]
    id: i64,
    name: String,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "order_lines", tenant = "org_id", soft_delete = "deleted_at")]
struct Line {
    #[entity(id)]
    id: i64,
    order_id: i64,
    qty: i64,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "tags", tenant = "org_id")]
struct Tag {
    #[entity(id)]
    id: i64,
    name: String,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "orders", tenant = "org_id")]
#[entity(belongs_to(Customer), has_many(Line), many_to_many(Tag, through = "order_tags"))]
struct Order {
    #[entity(id)]
    id: i64,
    customer_id: Option<i64>,
    title: String,
}

assert_eq!(Order::lines().kind(), RelationKind::HasMany { foreign_key: "order_id" });
assert_eq!(
    Order::tags().kind(),
    RelationKind::ManyToMany { through: "order_tags", local_key: "order_id", foreign_key: "tag_id" }
);
assert_eq!(Order::RELATIONS.iter().map(|r| r.name).collect::<Vec<_>>(), ["customer", "lines", "tags"]);
assert_eq!(Order::RELATIONS[0].target, "Customer");
```

Without the derive, `Relation::belongs_to(name, foreign_key)`, `Relation::has_many(name, foreign_key)` and `Relation::many_to_many(name, through, local_key, foreign_key)` build the same values for a hand-written `impl Entity`.

## Eager loading: `Repository::with`

`repo.with(Order::lines())` returns an `EagerRepository` with the repository's reads (`find`, `find_all`, `find_by`, `find_page`, and `load` for entities you already have, e.g. a keyset page). Each returns `Loaded<Order>` values: they deref to the order, and `get(&Order::lines())` returns that order's lines (`one(..)` the single parent of a `belongs_to`). Chain `with` for more relations; `on(executor)` runs them in a transaction as `Repository::on` does.

After the parent query, each relation runs **one** query with the keys of all parents in an `IN (…)` list (deduplicated; sent in chunks of 1 000 keys for very large reads). Each child row carries its owner's key as `__id_effect_key`, which is how the children are handed to their parents:

```rust
use id_effect::Exit;
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlRow, SqlValue, TestSqlClient, sql_client_env};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "customers", tenant = "org_id")]
struct Customer {
    #[entity(id)]
    id: i64,
    name: String,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "order_lines", tenant = "org_id")]
struct Line {
    #[entity(id)]
    id: i64,
    order_id: i64,
    qty: i64,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "orders", tenant = "org_id", belongs_to(Customer), has_many(Line))]
struct Order {
    #[entity(id)]
    id: i64,
    customer_id: Option<i64>,
    title: String,
}

let test = TestSqlClient::new();
let _ = test.connect().block_on();
let order = |id: i64| SqlRow::from_pairs([("id", SqlValue::I64(id)), ("customer_id", SqlValue::I64(3)), ("title", "t".into())]);
test.script_query(
    r#"SELECT "id", "customer_id", "title" FROM "orders" WHERE "org_id" = $1 ORDER BY "id" ASC"#,
    vec![order(1), order(2)],
);
let line = |id: i64, order_id: i64| SqlRow::from_pairs([
    ("id", id), ("order_id", order_id), ("qty", 1), ("__id_effect_key", order_id),
]);
test.script_query(
    r#"SELECT "id", "order_id", "qty", "order_id" AS "__id_effect_key" FROM "order_lines" WHERE "org_id" = $1 AND "order_id" IN ($2, $3) ORDER BY "id" ASC"#,
    vec![line(10, 1), line(11, 1), line(12, 2)],
);
test.script_query(
    r#"SELECT "id", "name", "id" AS "__id_effect_key" FROM "customers" WHERE "org_id" = $1 AND "id" IN ($2) ORDER BY "id" ASC"#,
    vec![SqlRow::from_pairs([("id", SqlValue::I64(3)), ("name", "ada".into()), ("__id_effect_key", SqlValue::I64(3))])],
);

let orders = Repository::<Order>::new(Dialect::Postgres).with_tenant(7i64);
let loaded = orders
    .with(Order::lines())
    .with(Order::customer())
    .find_all::<SqlClientEnv, _>()
    .provide_all(sql_client_env(test.clone()))
    .block_on();
let Exit::Success(loaded) = loaded else { panic!("{loaded:?}") };

let lines_of = |i: usize| loaded[i].get(&Order::lines()).iter().map(|l| l.id).collect::<Vec<_>>();
assert_eq!((lines_of(0), lines_of(1)), (vec![10, 11], vec![12]));
assert_eq!(loaded[1].one(&Order::customer()).map(|c| c.name.as_str()), Some("ada"));
assert_eq!(loaded[0].title, "t"); // Loaded<Order> derefs to the order

// Two orders, two relations: three queries, not five.
assert_eq!(test.recorded_queries().len(), 3);
```

### Nesting

`Relation::with` loads the relations of the children: `Order::lines().with(Line::product().with(Product::vendor()))` reads orders, lines, products and vendors in four queries. Chains are at most `MAX_EAGER_DEPTH` (3) relations deep; a deeper one fails with `SqlError::Unsupported` before any query runs, so an accidental fan-out never reaches the database.

## Secure by default: children keep their filters

A relation reads its children the way the parent repository reads (ADR 0013 R1): with the same tenant and filter mode, so the **child's** own global filters are AND-ed into the relation query (inside the join for `many_to_many`):

- a tenant-scoped child is filtered by the repository's tenant. A foreign key or join-table row that points at another tenant's row loads nothing: a relation can never return another tenant's data, however the rows were written;
- a soft-deleted child is hidden (`with_deleted()` on the repository shows it, as it does for the parents);
- a tenant-scoped child under a repository without a tenant fails closed (`SqlError::Unsupported`), even when the parent entity itself is not tenant-scoped;
- `unscoped()` lifts every filter of the parents and the children: the explicit escape hatch.

```rust
use id_effect::{Cause, Exit};
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlError, TestSqlClient, sql_client_env};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "tags", tenant = "org_id", soft_delete = "deleted_at")]
struct Tag {
    #[entity(id)]
    id: i64,
    name: String,
}

/// Posts are not tenant-scoped; their tags are.
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "posts", many_to_many(Tag, through = "post_tags"))]
struct Post {
    #[entity(id)]
    id: i64,
}

let test = TestSqlClient::new();
let _ = test.connect().block_on();
let no_tenant = Repository::<Post>::new(Dialect::Postgres)
    .with(Post::tags())
    .find_all::<SqlClientEnv, _>()
    .provide_all(sql_client_env(test.clone()))
    .block_on();
assert!(matches!(no_tenant, Exit::Failure(Cause::Fail(SqlError::Unsupported(_)))));
assert!(test.recorded_queries().is_empty(), "nothing was read");
```

`crates/id_effect_sql/tests/relations.rs` proves both rules on SQLite and `tests/repository_relations_pg.rs` on PostgreSQL: `m19_eager_loaded_children_scoped_to_tenant` (a line of tenant 2 pointing at an order of tenant 1, a customer of another tenant, soft-deleted lines) and `relation_join_cannot_cross_tenant` (join-table rows pairing an order with another tenant's tag).

## Field metadata

Generic tools (an admin, exports, form builders) need to know what an entity's fields are. The derive fills `Entity::FIELDS` with one `FieldMeta` per field: name, column, the type as written (without `Option`), `nullable`, `editable`, a label key and, for a `belongs_to` foreign key, the relation's name. `Entity::LABEL` names the entity.

- `#[entity(editable)]` marks a field a generic form may bind. Nothing is editable by default (no mass assignment), and the id, version, tenant and soft-delete columns can never be.
- `#[entity(label = "..")]` on a field or the struct sets the label key, e.g. a translation key; the defaults are `<entity>.<field>` and `<entity>` in snake case.

```rust
use id_effect_sql::repository::FieldMeta;
use id_effect_sql::{Entity, FromRow};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "customers")]
struct Customer {
    #[entity(id)]
    id: i64,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "invoices", label = "billing.invoice", belongs_to(Customer))]
struct Invoice {
    #[entity(id)]
    id: i64,
    customer_id: i64,
    #[entity(editable, label = "billing.invoice.memo")]
    memo: Option<String>,
}

assert_eq!(Invoice::LABEL, "billing.invoice");
assert_eq!(
    Invoice::FIELDS[1],
    FieldMeta::new("customer_id", "customer_id", "i64").label("invoice.customer_id").relation("customer")
);
let memo = Invoice::FIELDS[2];
assert_eq!((memo.schema_type, memo.nullable, memo.editable, memo.label), ("String", true, true, "billing.invoice.memo"));
```

Both constants default to empty in the `Entity` trait, so a hand-written `impl Entity` from 0.12 compiles unchanged (`manual_entity_impl_still_compiles`).

## Tests

- `crates/id_effect_sql/tests/relations.rs`: `eager_load_issues_one_query_per_relation` (a query-count probe on the executor), `many_to_many_through_join_table`, `nested_eager_load_three_levels`, `m19_eager_loaded_children_scoped_to_tenant`, `relation_join_cannot_cross_tenant`, `entity_fields_metadata_from_derive` and `manual_entity_impl_still_compiles`, on SQLite in memory: `cargo nextest run -p id_effect_sql --features repository,sqlite --test relations`.
- `tests/repository_relations_pg.rs` runs the same suite on PostgreSQL in its own container: `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_sql --features repository,pg --test repository_relations_pg`.
