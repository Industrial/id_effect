# Keyset pagination

[Repositories](ch28-03-repositories.md) page with an offset: `find_page(cond, Page::number(2, 20))` skips forty rows and reads twenty. That is right for numbered pages of a table that does not move, and wrong for a feed: when rows are inserted before the page a reader is on, every later page shifts, so the reader sees rows twice; when rows are deleted, they skip some. The database also reads and throws away every skipped row, so deep pages get slower.

**Keyset pagination** remembers *where* the last page ended instead of *how many rows* came before it: the next page is the rows after the last row's sort key. `id_effect_sql` (feature `repository`) reads such pages with `Repository::find_after` and `find_before`, and hands the position to the client as an opaque `PageCursor`.

## A keyset and its pages

A `Keyset` is the request: the sort columns, a filter and a page size.

- `order_by(column, SortOrder::Desc)` adds a sort column. Columns must be mapped columns of the entity — anything else is refused with `SqlError::Unsupported` before a query runs, so a sort column chosen by a user (an admin list, an API's `?sort=`) never reaches the SQL as text.
- The primary key is always the last sort column (ascending, unless you sorted on it yourself), so every row has exactly one position even when the declared columns tie.
- `filter(cond)` restricts the rows (AND-ed; the repository's tenant, soft-delete and custom filters are always added on top).
- `size(n)` asks for `n` rows per page (see [limits](#page-size-limits)).

`find_after(&keyset, None)` reads the first page; `find_after(&keyset, Some(cursor))` the page after `cursor`. Both return a `Paged<T>`:

| Field | What it is |
|---|---|
| `items` | the rows, in the keyset order |
| `next` | the cursor of the last row — pass it to `find_after` for the next page — or `None` on the last page |
| `prev` | the cursor of the first row — pass it to `find_before` for the previous page — or `None` on the first page |

The repository reads one row more than the page, so `next` is exact when paging forward (and `prev` when paging back with `find_before`); the other one is set whenever the page was read from a cursor. `find_before(&keyset, None)` reads the last page.

```rust
use id_effect::Exit;
use id_effect_sql::repository::{Keyset, SortOrder};
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlRow, SqlValue, TestSqlClient, sql_client_env};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "posts", tenant = "org_id")]
struct Post {
    #[entity(id)]
    id: i64,
    score: i64,
}

let post = |id: i64, score: i64| SqlRow::from_pairs([("id", SqlValue::I64(id)), ("score", SqlValue::I64(score))]);
let test = TestSqlClient::new();
let _ = test.connect().block_on();
// Two rows per page: the repository asks for three to know whether a next page exists.
test.script_query(
    r#"SELECT "id", "score" FROM "posts" WHERE "org_id" = $1 ORDER BY "score" DESC, "id" ASC LIMIT $2"#,
    vec![post(4, 9), post(2, 7), post(3, 7)],
);

let posts = Repository::<Post>::new(Dialect::Postgres).with_tenant(1i64);
let keyset = Keyset::new().order_by("score", SortOrder::Desc).size(2);
let page = posts
    .find_after::<SqlClientEnv, _>(&keyset, None)
    .provide_all(sql_client_env(test.clone()))
    .block_on();
let Exit::Success(page) = page else { panic!("{page:?}") };

assert_eq!(page.items.iter().map(|p| p.id).collect::<Vec<_>>(), [4, 2]);
assert!(page.prev.is_none());
let next = page.next.expect("a third row exists");
// The position is the last row's sort key: (score 7, id 2).
assert_eq!(next.keys(), [SqlValue::I64(7), SqlValue::I64(2)]);
assert_eq!(test.recorded_queries()[0].1, [SqlValue::I64(1), SqlValue::I64(3)]);

// The next page: the rows strictly after (7, 2).
test.script_query(
    r#"SELECT "id", "score" FROM "posts" WHERE "org_id" = $1 AND ("score" < $2 OR ("score" = $3 AND "id" > $4)) ORDER BY "score" DESC, "id" ASC LIMIT $5"#,
    vec![post(3, 7)],
);
let page2 = posts
    .find_after::<SqlClientEnv, _>(&keyset, Some(next))
    .provide_all(sql_client_env(test.clone()))
    .block_on();
let Exit::Success(page2) = page2 else { panic!("{page2:?}") };
assert_eq!(page2.items.iter().map(|p| p.id).collect::<Vec<_>>(), [3]);
assert!(page2.next.is_none(), "the last page");
assert!(page2.prev.is_some(), "read from a cursor: there is a page before it");
```

The page after `next` is read with `WHERE "org_id" = $1 AND ("score" < $2 OR ("score" = $3 AND "id" > $4))` — the rows strictly after (7, 2) in the order — so a post inserted with a score of 10 while the reader is on page one does not push a row of page one onto page two. A composite index on the sort columns and the primary key (`(org_id, score DESC, id)` here) makes each page an index range scan however deep it is.

`BoundRepository` (`repo.on(tx)`) has the same two methods, and `repo.with(Post::comments())` — an [`EagerRepository`](ch28-06-relations.md) — returns `Paged<Loaded<Post>>`, loading the relations of the page's rows in one query per relation. On an executor, page with the bound repository and pass the rows to `BoundEagerRepository::load`, then `Paged::with_items`.

## The cursor

A `PageCursor` is a position plus the identity of the request it was issued for (ADR 0015 D11). `encode()` (also `Display`) turns it into the token you put in a response — base64url without padding, of a small JSON document with the sort-key tuple and a 64-bit fingerprint — and `PageCursor::decode(token)` (also `FromStr`) reads one back from a request:

- a token longer than `MAX_CURSOR_BYTES` (1 KiB) is refused before it is decoded (`PageCursorError::TooLong`);
- the rest goes through strict base64url, JSON and a bounded `Schema`: version 1, a 16-digit hex fingerprint, 1 to `MAX_SORT_KEYS` (8) typed keys (booleans, integers, finite floats, text, bytes, UUIDs, dates and times, decimals — never NULL or JSON). Anything else is `PageCursorError::Malformed`;
- the **fingerprint** covers the entity's table, the repository's filter mode (`with_deleted`, `unscoped`), the sort and the keyset filter with its values. A page read with a cursor whose fingerprint differs from the request's fails with `SqlError::Decode` (`PageCursorError::FilterMismatch`) before any query runs: a cursor taken for "open posts by score" cannot page "draft posts" or "posts by title". The page size is not part of it, so a client may change it between pages.

```rust
use id_effect::{Cause, Exit};
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait};
use id_effect_sql::repository::{Keyset, PageCursor, PageCursorError};
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlError, SqlValue, TestSqlClient, sql_client_env};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "posts")]
struct Post {
    #[entity(id)]
    id: i64,
    status: String,
}

// Oversize tokens are refused before decoding; garbage is malformed.
assert!(matches!(PageCursor::decode(&"A".repeat(1025)), Err(PageCursorError::TooLong { len: 1025 })));
assert!(matches!(PageCursor::decode("not a cursor"), Err(PageCursorError::Malformed(_))));

// A cursor issued for "open" posts (forged here with the fingerprint of another request).
let open = Keyset::new().filter(Expr::col("status").eq("open"));
let cursor = PageCursor::new(0x1234, vec![SqlValue::I64(10)]).unwrap();
let token = cursor.encode();
assert_eq!(PageCursor::decode(&token), Ok(cursor.clone()));

let test = TestSqlClient::new();
let _ = test.connect().block_on();
let read = Repository::<Post>::new(Dialect::Postgres)
    .find_after::<SqlClientEnv, _>(&open, Some(cursor))
    .provide_all(sql_client_env(test.clone()))
    .block_on();
assert!(matches!(read, Exit::Failure(Cause::Fail(SqlError::Decode(m))) if m.contains("fingerprint mismatch")));
assert!(test.recorded_queries().is_empty(), "refused before any query");
```

The cursor is **not signed**. A client can edit the keys, and that is harmless: every page query re-applies the repository's tenant, soft-delete and custom filters, so a forged position only moves the page inside rows the caller may read anyway. Signing would add a key to rotate without closing any hole. (`PageCursor::new(fingerprint, keys)` builds a position directly, with the same checks as `decode`; the tests use it to forge cursors.)

Sort columns should be `NOT NULL`. A NULL compares as unknown, so it has no position: when the row a cursor would point at has a NULL sort key, the page fails with `SqlError::Unsupported` instead of silently skipping rows. Likewise, a row whose sort key would make a cursor over 1 KiB (sorting by a long text column) fails the page rather than issuing a cursor `decode` would refuse.

## Page size limits

A page size comes from the client, so it is bounded (ADR 0013 R4): `PageLimits::default()` serves 50 rows when none is asked for and **caps** larger requests at `MAX_PAGE_SIZE` (500) — capped, not refused, so a client asking for 10 000 gets 500 and a `next` cursor. `size(0)` is one row. `Keyset::limits(PageLimits::new(default, max))` changes both; `max` is a `u32`, so the limit is always finite.

```rust
use id_effect_sql::repository::{Keyset, PageLimits};

assert_eq!(Keyset::new().page_size(), 50);
assert_eq!(Keyset::new().size(10_000).page_size(), 500);
assert_eq!(Keyset::new().size(0).page_size(), 1);
assert_eq!(Keyset::new().limits(PageLimits::new(25, 1_000)).size(u32::MAX).page_size(), 1_000);
```

## Offset or keyset

| | `find_page(cond, Page)` | `find_after(&Keyset, cursor)` |
|---|---|---|
| Jump to page *n* | yes | no (walk with the cursors) |
| Stable while rows are inserted or deleted | no | yes |
| Cost of a deep page | reads every skipped row | one index range scan |
| What the client holds | a page number | an opaque cursor (≤ 1 KiB) |
| Order | primary key | declared columns, then the primary key |

## Tests

`crates/id_effect_sql/tests/pagination.rs`, on SQLite in memory (`cargo nextest run -p id_effect_sql --features repository,sqlite --test pagination`):

- `keyset_stable_under_concurrent_inserts` — pages a feed while another task inserts rows on both sides of the cursor: no row twice, none of the original rows skipped (and the offset pages of the same data repeat rows);
- `keyset_prev_and_next_round_trip` — forward to the end and back through the encoded tokens, including `find_before(None)` and a bound repository;
- `keyset_with_eager_relations` — `EagerRepository::find_after` / `find_before`, children scoped to the tenant, one query per relation;
- `r4_page_cursor_over_limit_rejected`, `r4_page_size_capped`, `cursor_with_other_filter_refused` (other filter, sort or filter mode; unknown, repeated or NULL sort columns) and `tampered_cursor_stays_in_scope` (forged positions read only the caller's live rows).

`PageCursor::decode` is public so the `sql_page_cursor` fuzz target can drive it with arbitrary bytes.
