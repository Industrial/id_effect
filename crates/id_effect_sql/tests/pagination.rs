//! Keyset (cursor) pagination (feature `repository`, E3.3 "pagination", ADR 0015 D11):
//! `Repository::find_after` / `find_before`, `Paged { items, next, prev }`, the opaque
//! `PageCursor` and its limits, on SQLite in memory (feature `sqlite`).
//!
//! ADR 0013 R4: `r4_page_cursor_over_limit_rejected`, `r4_page_size_capped`; R1:
//! `cursor_with_other_filter_refused`, `tampered_cursor_stays_in_scope`.

#![cfg(all(feature = "repository", feature = "sqlite"))]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use id_effect::{Cause, Effect, Exit, hlist};
use id_effect_sql::query_builder::sea_query::{Condition, Expr, ExprTrait};
use id_effect_sql::repository::page::{MAX_CURSOR_BYTES, MAX_PAGE_SIZE};
use id_effect_sql::repository::{
  Keyset, Loaded, PageCursor, PageCursorError, PageLimits, Paged, SortOrder,
};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
use id_effect_sql::{
  Dialect, Entity, FromRow, Page, Repository, SqlClient, SqlClientEnv, SqlClientService, SqlError,
  SqlExecutor, SqlRow, SqlValue,
};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "pg_posts", tenant = "org_id", soft_delete = "deleted_at")]
#[entity(has_many(Comment))]
pub struct Post {
  #[entity(id)]
  pub id: i64,
  pub score: i64,
  pub title: String,
  pub status: String,
  pub rank: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "pg_comments", tenant = "org_id")]
pub struct Comment {
  #[entity(id)]
  pub id: i64,
  pub post_id: i64,
  pub body: String,
}

/// One in-memory database.
struct Db {
  client: Arc<dyn SqlClient>,
}

impl Db {
  async fn new() -> Self {
    let pool = sqlite_pool_from_config(SqlitePoolConfig::memory().with_max_size(4))
      .await
      .expect("sqlite pool");
    let db = Self {
      client: Arc::new(SqliteSqlClient::new(pool)),
    };
    db.exec(
      "CREATE TABLE pg_posts (id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, \
       score INTEGER NOT NULL, title TEXT NOT NULL, status TEXT NOT NULL, rank INTEGER, \
       deleted_at TEXT)",
    )
    .await;
    db.exec(
      "CREATE TABLE pg_comments (id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, \
       post_id INTEGER NOT NULL, body TEXT NOT NULL)",
    )
    .await;
    db
  }

  async fn plain<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
    match id_effect_tokio::TokioRuntime::current()
      .expect("tokio")
      .run(eff)
      .await
    {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  async fn run<A: Send + 'static>(
    &self,
    eff: Effect<A, SqlError, SqlClientEnv>,
  ) -> Result<A, SqlError> {
    Self::plain(eff.provide_all(hlist![SqlClientService(Arc::clone(&self.client))])).await
  }

  async fn page(&self, eff: Effect<Paged<Post>, SqlError, SqlClientEnv>) -> Paged<Post> {
    self
      .run(eff)
      .await
      .unwrap_or_else(|e| panic!("page failed: {e}"))
  }

  async fn exec(&self, sql: &str) {
    Self::plain(SqlClient::execute(&*self.client, sql, &[]))
      .await
      .unwrap_or_else(|e| panic!("{sql}: {e}"));
  }

  /// Post `id` of `org` with `score`.
  async fn post(&self, org: i64, id: i64, score: i64) {
    self
      .exec(&format!(
        "INSERT INTO pg_posts (id, org_id, score, title, status) \
         VALUES ({id}, {org}, {score}, 'post {id}', '{}')",
        if id % 2 == 0 { "open" } else { "draft" }
      ))
      .await;
  }
}

fn posts(org: i64) -> Repository<Post> {
  Repository::<Post>::new(Dialect::Sqlite).with_tenant(org)
}

fn ids(page: &Paged<Post>) -> Vec<i64> {
  page.items.iter().map(|p| p.id).collect()
}

fn by_score() -> Keyset {
  Keyset::new().order_by("score", SortOrder::Desc)
}

/// An executor counting the queries it runs.
#[derive(Clone)]
struct Probe {
  inner: Arc<dyn SqlClient>,
  queries: Arc<Mutex<Vec<String>>>,
}

impl SqlExecutor for Probe {
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    self.queries.lock().expect("probe").push(sql.to_owned());
    SqlClient::query(&*self.inner, sql, params)
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    SqlClient::execute(&*self.inner, sql, params)
  }
}

// ---- functional ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keyset_stable_under_concurrent_inserts() {
  let db = Db::new().await;
  // Twenty posts with tied scores (the primary key breaks the ties).
  for id in 1..=20 {
    db.post(1, id, id % 4).await;
  }
  let keyset = by_score().size(6);
  let first = db.page(posts(1).find_after(&keyset, None)).await;
  assert_eq!(ids(&first), [3, 7, 11, 15, 19, 2]);

  // The offset pages (primary-key order) for contrast.
  let offset_first = db
    .run(posts(1).find_page(Condition::all(), Page::number(0, 6)))
    .await
    .unwrap();

  // Higher-scored posts with lower ids land before both positions: an offset now repeats rows.
  for id in -5..=-1 {
    db.post(1, id, 10).await;
  }
  let offset = db
    .run(posts(1).find_page(Condition::all(), Page::number(1, 6)))
    .await
    .unwrap();
  assert!(
    offset.iter().any(|p| offset_first.contains(p)),
    "offset page 2 repeats rows of page 1: {:?}",
    offset.iter().map(|p| p.id).collect::<Vec<_>>()
  );

  // Keep paging while another task inserts posts on both sides of the cursor.
  let writer = {
    let client = Arc::clone(&db.client);
    tokio::spawn(async move {
      for id in 200..240 {
        let post = Post {
          id,
          score: id % 5,
          title: format!("late {id}"),
          status: "open".into(),
          rank: None,
        };
        Db::plain(posts(1).on(Arc::clone(&client)).insert(post))
          .await
          .expect("insert");
        tokio::task::yield_now().await;
      }
    })
  };
  let mut seen: Vec<i64> = ids(&first);
  let mut cursor = first.next.clone();
  while let Some(at) = cursor {
    // The client carries the token, not the value.
    let at = PageCursor::decode(&at.encode()).expect("own cursor");
    let page = db.page(posts(1).find_after(&keyset, Some(at))).await;
    assert!(page.items.len() <= 6);
    assert!(
      page.prev.is_some(),
      "a page read from a cursor has a previous page"
    );
    seen.extend(ids(&page));
    cursor = page.next;
  }
  writer.await.expect("writer");

  let unique: HashSet<i64> = seen.iter().copied().collect();
  assert_eq!(unique.len(), seen.len(), "no row twice: {seen:?}");
  for id in 1..=20 {
    assert!(unique.contains(&id), "row {id} skipped: {seen:?}");
  }
  for id in -5..=-1 {
    assert!(
      !unique.contains(&id),
      "rows inserted before the cursor stay behind it"
    );
  }
  // Pages are in the keyset order: score descending, then id ascending.
  let order: Vec<(i64, i64)> = seen
    .iter()
    .map(|id| (if *id >= 200 { id % 5 } else { id % 4 }, *id))
    .collect();
  let mut sorted = order.clone();
  sorted.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
  assert_eq!(order, sorted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keyset_prev_and_next_round_trip() {
  let db = Db::new().await;
  for id in 1..=10 {
    db.post(1, id, id % 3).await;
  }
  let keyset = by_score().order_by("title", SortOrder::Asc).size(3);
  let repo = posts(1);

  // Forward to the end.
  let mut pages = vec![db.page(repo.find_after(&keyset, None)).await];
  while let Some(next) = pages.last().and_then(|p| p.next.clone()) {
    pages.push(db.page(repo.find_after(&keyset, Some(next))).await);
  }
  let forward: Vec<Vec<i64>> = pages.iter().map(ids).collect();
  // score 2: 2,5,8; score 1: 1,10,4,7 (by title: "post 1" < "post 10" < "post 4" < "post 7");
  // score 0: 3,6,9.
  assert_eq!(
    forward,
    [vec![2, 5, 8], vec![1, 10, 4], vec![7, 3, 6], vec![9]]
  );
  assert!(pages[0].prev.is_none() && pages[3].next.is_none());
  assert_eq!(
    pages[1].next.as_ref().map(|c| c.keys().to_vec()),
    Some(vec![
      SqlValue::I64(1),
      SqlValue::Text("post 4".into()),
      SqlValue::I64(4)
    ])
  );

  // And back from the last page, through the opaque tokens.
  let mut back = vec![forward[3].clone()];
  let mut prev = pages[3].prev.as_ref().map(PageCursor::encode);
  let mut last = None;
  while let Some(token) = prev {
    let at: PageCursor = token.parse().expect("own cursor");
    let page = db.page(repo.find_before(&keyset, Some(at))).await;
    assert!(
      page.next.is_some(),
      "a page read back from a cursor has a next page"
    );
    back.insert(0, ids(&page));
    prev = page.prev.as_ref().map(PageCursor::encode);
    last = Some(page);
  }
  assert_eq!(back, forward);
  assert_eq!(last.expect("pages").next, pages[0].next);

  // Without a cursor, `find_before` reads the last page.
  let tail = db.page(repo.find_before(&keyset, None)).await;
  assert_eq!(ids(&tail), [3, 6, 9]);
  assert!(tail.next.is_none() && tail.prev.is_some());

  // A transaction or any executor: the bound repository pages the same way.
  let bound = repo.on(Arc::clone(&db.client));
  let again = Db::plain(bound.find_after(&keyset, pages[0].next.clone()))
    .await
    .unwrap();
  assert_eq!(ids(&again), forward[1]);
  let before = Db::plain(bound.find_before(&keyset, pages[1].prev.clone()))
    .await
    .unwrap();
  assert_eq!(ids(&before), forward[0]);
  assert!(before.prev.is_none());

  // Past the end: an empty page with no cursors.
  let end = PageCursor::new(
    pages[0].next.as_ref().unwrap().fingerprint(),
    vec![
      SqlValue::I64(-1),
      SqlValue::Text(String::new()),
      SqlValue::I64(0),
    ],
  )
  .unwrap();
  let empty = db.page(repo.find_after(&keyset, Some(end))).await;
  assert!(empty.is_empty() && empty.next.is_none() && empty.prev.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keyset_with_eager_relations() {
  let db = Db::new().await;
  for id in 1..=5 {
    db.post(1, id, id).await;
  }
  db.exec(
    "INSERT INTO pg_comments (id, org_id, post_id, body) VALUES \
     (1, 1, 5, 'a'), (2, 1, 5, 'b'), (3, 1, 4, 'c'), (4, 1, 1, 'd'), (5, 2, 5, 'other tenant')",
  )
  .await;
  let keyset = by_score().size(2);
  let comments = Post::comments();
  let bodies = |p: &Loaded<Post>| {
    p.get(&comments)
      .iter()
      .map(|c| c.body.clone())
      .collect::<Vec<_>>()
  };

  let page = db
    .run(posts(1).with(Post::comments()).find_after(&keyset, None))
    .await
    .unwrap();
  assert_eq!(page.items.iter().map(|p| p.id).collect::<Vec<_>>(), [5, 4]);
  assert_eq!(
    bodies(&page.items[0]),
    ["a", "b"],
    "children re-scoped by tenant"
  );
  assert_eq!(bodies(&page.items[1]), ["c"]);
  let page2 = db
    .run(
      posts(1)
        .with(Post::comments())
        .find_after(&keyset, page.next.clone()),
    )
    .await
    .unwrap();
  assert_eq!(page2.items.iter().map(|p| p.id).collect::<Vec<_>>(), [3, 2]);
  let back = db
    .run(
      posts(1)
        .with(Post::comments())
        .find_before(&keyset, page2.prev.clone()),
    )
    .await
    .unwrap();
  assert_eq!(back.items.iter().map(|p| p.id).collect::<Vec<_>>(), [5, 4]);
  assert_eq!(back.next, page.next);

  // On an executor: the page, then its relations — one query each, no N+1.
  let probe = Probe {
    inner: Arc::clone(&db.client),
    queries: Arc::default(),
  };
  let repo = posts(1);
  let rows = Db::plain(repo.on(probe.clone()).find_after(&keyset, None))
    .await
    .unwrap();
  let items = Db::plain(
    repo
      .with(Post::comments())
      .on(probe.clone())
      .load(rows.items.clone()),
  )
  .await
  .unwrap();
  let loaded = rows.with_items(items);
  assert_eq!(bodies(&loaded.items[0]), ["a", "b"]);
  assert_eq!(
    probe.queries.lock().unwrap().len(),
    2,
    "one page query, one relation query"
  );
}

// ---- secure by default (ADR 0013 R1, R4) ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r4_page_cursor_over_limit_rejected() {
  // A token over 1 KiB is refused before it is decoded.
  let long = "A".repeat(MAX_CURSOR_BYTES + 1);
  assert_eq!(
    PageCursor::decode(&long),
    Err(PageCursorError::TooLong {
      len: MAX_CURSOR_BYTES + 1
    })
  );
  // At the limit it is decoded (and here refused as malformed, not as too long).
  assert!(matches!(
    PageCursor::decode(&"A".repeat(MAX_CURSOR_BYTES)),
    Err(PageCursorError::Malformed(_))
  ));
  let err: SqlError = PageCursorError::TooLong { len: 2048 }.into();
  assert!(
    matches!(&err, SqlError::Decode(m) if m.contains("1024-byte limit")),
    "{err}"
  );

  // A row whose sort key would make a cursor over the limit fails the page; no cursor is issued.
  let db = Db::new().await;
  db.post(1, 1, 1).await;
  db.post(1, 2, 1).await;
  db.exec(&format!(
    "UPDATE pg_posts SET title = '{}' WHERE id = 1",
    "a".repeat(MAX_CURSOR_BYTES)
  ))
  .await;
  let keyset = Keyset::new().order_by("title", SortOrder::Asc).size(1);
  let err = db
    .run(posts(1).find_after(&keyset, None))
    .await
    .unwrap_err();
  assert!(
    matches!(&err, SqlError::Unsupported(m) if m.contains("over the 1024-byte limit")),
    "{err}"
  );
  // Short keys page normally.
  let page = db
    .page(posts(1).find_after(&Keyset::new().size(1), None))
    .await;
  assert!(page.next.expect("next").encode().len() <= MAX_CURSOR_BYTES);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cursor_with_other_filter_refused() {
  let db = Db::new().await;
  for id in 1..=6 {
    db.post(1, id, id).await;
  }
  let open = by_score().filter(Expr::col("status").eq("open")).size(1);
  let page = db.page(posts(1).find_after(&open, None)).await;
  assert_eq!(ids(&page), [6]);
  let cursor = page.next.expect("next");

  let fingerprint_mismatch = |r: Result<Paged<Post>, SqlError>| matches!(r, Err(SqlError::Decode(m)) if m.contains("fingerprint mismatch"));
  // Another filter value, another filter, another sort, another filter mode: refused.
  let draft = by_score().filter(Expr::col("status").eq("draft")).size(1);
  let unfiltered = by_score().size(1);
  let by_title = Keyset::new()
    .order_by("title", SortOrder::Desc)
    .filter(Expr::col("status").eq("open"));
  let ascending = Keyset::new()
    .order_by("score", SortOrder::Asc)
    .filter(Expr::col("status").eq("open"));
  for keyset in [&draft, &unfiltered, &by_title, &ascending] {
    assert!(
      fingerprint_mismatch(
        db.run(posts(1).find_after(keyset, Some(cursor.clone())))
          .await
      ),
      "{keyset:?}"
    );
  }
  assert!(fingerprint_mismatch(
    db.run(
      posts(1)
        .with_deleted()
        .find_after(&open, Some(cursor.clone()))
    )
    .await
  ));
  assert!(fingerprint_mismatch(
    db.run(posts(1).find_before(&draft, Some(cursor.clone())))
      .await
  ));
  // The page size is not part of the request's identity.
  let wider = by_score().filter(Expr::col("status").eq("open")).size(5);
  let next = db
    .page(posts(1).find_after(&wider, Some(cursor.clone())))
    .await;
  assert_eq!(ids(&next), [4, 2]);

  // A forged cursor with the right fingerprint but the wrong number of keys.
  let short = PageCursor::new(cursor.fingerprint(), vec![SqlValue::I64(1)]).unwrap();
  let err = db
    .run(posts(1).find_after(&open, Some(short)))
    .await
    .unwrap_err();
  assert!(
    matches!(&err, SqlError::Decode(m) if m.contains("2 columns")),
    "{err}"
  );

  // Sort columns must be mapped columns: nothing else reaches the SQL.
  for keyset in [
    Keyset::new().order_by("org_id", SortOrder::Asc),
    Keyset::new().order_by("score\" DESC; --", SortOrder::Asc),
    by_score().order_by("score", SortOrder::Asc),
    ["id", "score", "title", "status", "rank", "id"]
      .iter()
      .fold(Keyset::new(), |k, c| k.order_by(*c, SortOrder::Asc)),
  ] {
    let err = db
      .run(posts(1).find_after(&keyset, None))
      .await
      .unwrap_err();
    assert!(matches!(err, SqlError::Unsupported(_)), "{err}");
  }

  // A NULL sort key cannot be a cursor position: the page fails closed.
  db.exec("UPDATE pg_posts SET rank = id WHERE id > 1").await;
  let by_rank = Keyset::new().order_by("rank", SortOrder::Desc).size(5);
  let page = db.page(posts(1).find_after(&by_rank, None)).await;
  assert_eq!(ids(&page), [6, 5, 4, 3, 2]);
  // SQLite sorts NULL first ascending: the first row of the page is the NULL one.
  let by_rank = Keyset::new().order_by("rank", SortOrder::Asc).size(1);
  let err = db
    .run(posts(1).find_after(&by_rank, None))
    .await
    .unwrap_err();
  assert!(
    matches!(&err, SqlError::Unsupported(m) if m.contains("NULL")),
    "{err}"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r4_page_size_capped() {
  let db = Db::new().await;
  db.exec(
    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 600) \
     INSERT INTO pg_posts (id, org_id, score, title, status) \
     SELECT i, 1, i % 7, 'post ' || i, 'open' FROM n",
  )
  .await;
  assert_eq!(MAX_PAGE_SIZE, 500);

  let default = db.page(posts(1).find_after(&Keyset::new(), None)).await;
  assert_eq!(default.items.len(), 50, "default page size");
  let huge = Keyset::new().size(10_000);
  assert_eq!(huge.page_size(), 500);
  let capped = db.page(posts(1).find_after(&huge, None)).await;
  assert_eq!(capped.items.len(), 500, "capped, not refused");
  assert!(capped.next.is_some());
  let rest = db.page(posts(1).find_after(&huge, capped.next)).await;
  assert_eq!(rest.items.len(), 100);
  assert!(rest.next.is_none());
  let zero = db
    .page(posts(1).find_after(&Keyset::new().size(0), None))
    .await;
  assert_eq!(zero.items.len(), 1, "at least one row per page");

  // The limit is configurable, and finite.
  let wide = Keyset::new()
    .limits(PageLimits::new(50, 1_000))
    .size(u32::MAX);
  assert_eq!(wide.page_size(), 1_000);
  let all = db.page(posts(1).find_after(&wide, None)).await;
  assert_eq!(all.items.len(), 600);
  assert!(all.next.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_cursor_stays_in_scope() {
  let db = Db::new().await;
  for id in 1..=5 {
    db.post(1, id, id).await;
  }
  for id in 6..=10 {
    db.post(2, id, id).await;
  }
  db.post(1, 11, 11).await;
  db.exec("UPDATE pg_posts SET deleted_at = '2024-01-01T00:00:00Z' WHERE id = 11")
    .await;

  let keyset = Keyset::new().size(3);
  let first = db.page(posts(1).find_after(&keyset, None)).await;
  assert_eq!(ids(&first), [1, 2, 3]);
  let fingerprint = first.next.as_ref().expect("next").fingerprint();
  // Forge positions: the request's fingerprint, any keys a client likes.
  let at = |id: i64| PageCursor::new(fingerprint, vec![SqlValue::I64(id)]).expect("cursor");

  let mut seen = Vec::new();
  for (from, back) in [
    (0, false),
    (3, false),
    (5, false),
    (7, false),
    (100, true),
    (-5, true),
  ] {
    let read = if back {
      posts(1).find_before(&keyset, Some(at(from)))
    } else {
      posts(1).find_after(&keyset, Some(at(from)))
    };
    seen.extend(ids(&db.page(read).await));
  }
  assert!(
    seen.iter().all(|id| (1..=5).contains(id)),
    "only tenant 1's live rows: {seen:?}"
  );
  // Tenant 2 forging from tenant 1's cursor still reads only its own rows.
  let theirs = db.page(posts(2).find_after(&keyset, Some(at(0)))).await;
  assert_eq!(ids(&theirs), [6, 7, 8]);
  // Through a forged token too: decoding grants nothing the filters do not.
  let token = at(5).encode();
  let page = db
    .page(posts(1).find_after(&keyset, Some(PageCursor::decode(&token).unwrap())))
    .await;
  assert!(
    page.is_empty(),
    "nothing of tenant 2 nor the soft-deleted row"
  );
  // Unscoped is the explicit escape hatch (and a different filter mode, so a new request).
  let all = Keyset::new().size(20);
  let everything = db.page(posts(1).unscoped().find_after(&all, None)).await;
  assert_eq!(everything.items.len(), 11);
}
