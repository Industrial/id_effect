//! [`PgTodoRepository`]: the [`TodoRepository`] port on PostgreSQL.

use std::sync::Arc;

use id_effect::{Effect, Or, fail, succeed};
use id_effect_sql::{SqlClientService, SqlTransaction, params, with_transaction};

use super::{BatchAbort, TodoRow, UniqueViolation};
use crate::domain::todo::{
  StoreError, Title, TitleTaken, Todo, TodoId, TodoNotFound, TodoRepository,
};

/// A [`TodoRepository`] on the [`SqlClientService`] (PostgreSQL SQL: `RETURNING`,
/// `ON CONFLICT`), normally the pool of [`database_layer`](super::database_layer).
///
/// Title uniqueness is the table's `UNIQUE` constraint: an insert uses `ON CONFLICT DO NOTHING`
/// (no row returned means the title is taken), an update maps the constraint's violation
/// (recognised from PostgreSQL's message). [`TodoRepository::add_all`] runs in one transaction
/// ([`with_transaction`]), so one taken title rolls back every insert of the batch. Run
/// [`Migration::apply`](super::Migration::apply) on the client first (the
/// [`PgTodoRepositoryLive`](super::PgTodoRepositoryLive) layer does).
#[derive(Clone, Debug)]
pub struct PgTodoRepository {
  sql: SqlClientService,
}

impl PgTodoRepository {
  /// Every todo, by id.
  pub const SELECT_ALL: &'static str = "SELECT id, title, completed FROM todos ORDER BY id";

  /// The todo `$1`.
  pub const SELECT_ONE: &'static str = "SELECT id, title, completed FROM todos WHERE id = $1";

  /// A new open todo titled `$1`; no row when the title is taken.
  pub const INSERT: &'static str = "INSERT INTO todos (title) VALUES ($1) \
                                    ON CONFLICT (title) DO NOTHING \
                                    RETURNING id, title, completed";

  /// The whole state of todo `$1`; no row when there is none.
  pub const UPDATE: &'static str = "UPDATE todos SET title = $2, completed = $3 WHERE id = $1 \
                                    RETURNING id, title, completed";

  /// Removes todo `$1`.
  pub const DELETE: &'static str = "DELETE FROM todos WHERE id = $1";

  /// The repository over `sql` (migrated: [`Migration::apply`](super::Migration::apply)).
  pub fn new(sql: SqlClientService) -> Self {
    Self { sql }
  }

  /// `result` as an effect.
  fn lift<A: Send + 'static, E: Send + 'static>(result: Result<A, E>) -> Effect<A, E> {
    match result {
      Ok(a) => succeed(a),
      Err(e) => fail(e),
    }
  }

  /// One [`Self::INSERT`] inside the batch transaction `tx`: a taken title aborts it.
  fn insert_in(tx: &Arc<dyn SqlTransaction>, title: Title) -> Effect<Todo, BatchAbort> {
    tx.query(Self::INSERT, &params![title.as_str()])
      .map_err(BatchAbort::from)
      .flat_map(move |rows| {
        Self::lift(match TodoRow::decode_first(&rows) {
          Ok(Some(todo)) => Ok(todo),
          Ok(None) => Err(BatchAbort::Taken(TitleTaken(title))),
          Err(err) => Err(BatchAbort::Store(err)),
        })
      })
  }
}

impl TodoRepository for PgTodoRepository {
  fn all(&self) -> Effect<Vec<Todo>, StoreError> {
    self
      .sql
      .query(Self::SELECT_ALL, &[])
      .map_err(StoreError::from)
      .flat_map(|rows| Self::lift(TodoRow::decode_all(&rows)))
  }

  fn find(&self, id: TodoId) -> Effect<Option<Todo>, StoreError> {
    self
      .sql
      .query(Self::SELECT_ONE, &params![id.get()])
      .map_err(StoreError::from)
      .flat_map(|rows| Self::lift(TodoRow::decode_first(&rows)))
  }

  fn add(&self, title: Title) -> Effect<Result<Todo, TitleTaken>, StoreError> {
    self
      .sql
      .query(Self::INSERT, &params![title.as_str()])
      .map_err(StoreError::from)
      .flat_map(move |rows| {
        Self::lift(TodoRow::decode_first(&rows).map(|todo| todo.ok_or(TitleTaken(title))))
      })
  }

  fn add_all(&self, titles: Vec<Title>) -> Effect<Result<Vec<Todo>, TitleTaken>, StoreError> {
    // One transaction: a taken title fails the bracket, which rolls every insert back.
    let batch = with_transaction(Arc::clone(&self.sql.0), move |tx| {
      titles.into_iter().fold(
        succeed(Vec::new()),
        move |done: Effect<Vec<Todo>, BatchAbort>, title| {
          let tx = Arc::clone(&tx);
          done.flat_map(move |mut todos| {
            Self::insert_in(&tx, title).map(move |todo| {
              todos.push(todo);
              todos
            })
          })
        },
      )
    });
    BatchAbort::settle(batch)
  }

  fn save(&self, todo: Todo) -> Effect<Result<Todo, Or<TodoNotFound, TitleTaken>>, StoreError> {
    let (id, title) = (todo.id(), todo.title().clone());
    let params = params![id.get(), title.as_str(), todo.is_completed()];
    self
      .sql
      .query(Self::UPDATE, &params)
      .map(Ok::<_, TitleTaken>)
      .catch(move |err| {
        if UniqueViolation::matches(&err) {
          succeed(Err(TitleTaken(title)))
        } else {
          fail(StoreError::from(err))
        }
      })
      .flat_map(move |outcome| {
        Self::lift(match outcome {
          Ok(rows) => {
            TodoRow::decode_first(&rows).map(|todo| todo.ok_or(Or::Left(TodoNotFound(id))))
          }
          Err(taken) => Ok(Err(Or::Right(taken))),
        })
      })
  }

  fn remove(&self, id: TodoId) -> Effect<bool, StoreError> {
    self
      .sql
      .execute(Self::DELETE, &params![id.get()])
      .map_err(StoreError::from)
      .map(|removed| removed > 0)
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};
  use id_effect_sql::{SqlClient as _, SqlRow, SqlValue, TestSqlClient};

  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, StoreError>) -> Result<A, StoreError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn title(raw: &str) -> Title {
    Title::parse(raw).unwrap()
  }

  fn row(id: i64, title: &str, completed: bool) -> SqlRow {
    SqlRow::from_pairs([
      ("id", SqlValue::I64(id)),
      ("title", SqlValue::Text(title.into())),
      ("completed", SqlValue::Bool(completed)),
    ])
  }

  /// A connected scripted client and a repository over it.
  fn scripted() -> (TestSqlClient, PgTodoRepository) {
    let client = TestSqlClient::new();
    assert!(matches!(client.connect().block_on(), Exit::Success(())));
    let repo = PgTodoRepository::new(SqlClientService::new(client.clone()));
    (client, repo)
  }

  #[test]
  fn reads_decode_the_returned_rows() {
    let (client, repo) = scripted();
    assert_eq!(run(repo.find(TodoId::new(1))).unwrap(), None);
    client.script_query(PgTodoRepository::SELECT_ALL, vec![row(1, "a", false)]);
    client.script_query(PgTodoRepository::SELECT_ONE, vec![row(1, "a", true)]);
    assert_eq!(run(repo.all()).unwrap().len(), 1);
    let found = run(repo.find(TodoId::new(1))).unwrap().unwrap();
    assert!(found.is_completed());
    let (_, params) = client.recorded_queries().pop().unwrap();
    assert_eq!(params, [SqlValue::I64(1)]);
  }

  #[test]
  fn a_corrupt_row_is_internal() {
    let (client, repo) = scripted();
    client.script_query(PgTodoRepository::SELECT_ALL, vec![row(1, " ", false)]);
    assert!(matches!(run(repo.all()), Err(StoreError::Internal(_))));
  }

  #[test]
  fn add_without_a_returned_row_is_a_taken_title() {
    let (client, repo) = scripted();
    assert_eq!(
      run(repo.add(title("a"))).unwrap(),
      Err(TitleTaken(title("a")))
    );
    client.script_query(PgTodoRepository::INSERT, vec![row(4, "a", false)]);
    let todo = run(repo.add(title("a"))).unwrap().unwrap();
    assert_eq!(todo.id(), TodoId::new(4));
  }

  #[test]
  fn add_all_rolls_back_on_a_taken_title() {
    let (client, repo) = scripted();
    assert_eq!(
      run(repo.add_all(vec![title("a"), title("b")])).unwrap(),
      Err(TitleTaken(title("a")))
    );
    assert_eq!((client.commits(), client.rollbacks()), (0, 1));
    assert!(client.recorded_queries().is_empty(), "rolled back");
    client.script_query(PgTodoRepository::INSERT, vec![row(5, "c", false)]);
    let made = run(repo.add_all(vec![title("c"), title("c")]))
      .unwrap()
      .unwrap();
    assert_eq!(made.len(), 2);
    assert_eq!(client.commits(), 1);
  }

  #[test]
  fn save_reports_not_found_and_taken_titles() {
    let (client, repo) = scripted();
    let todo = Todo::restore(TodoId::new(9), title("a"), true);
    assert_eq!(
      run(repo.save(todo.clone())).unwrap(),
      Err(Or::Left(TodoNotFound(TodoId::new(9))))
    );
    let (_, params) = client.recorded_queries().pop().unwrap();
    assert_eq!(
      params,
      [
        SqlValue::I64(9),
        SqlValue::Text("a".into()),
        SqlValue::Bool(true)
      ]
    );
    client.script_query(PgTodoRepository::UPDATE, vec![row(9, "a", true)]);
    assert_eq!(run(repo.save(todo.clone())).unwrap(), Ok(todo.clone()));
    client.fail_queries(format!(
      "db error: {} \"todos_title_key\"",
      UniqueViolation::MESSAGE
    ));
    assert_eq!(
      run(repo.save(todo.clone())).unwrap(),
      Err(Or::Right(TitleTaken(title("a"))))
    );
    client.fail_queries("deadlock detected");
    assert!(matches!(run(repo.save(todo)), Err(StoreError::Internal(_))));
  }

  #[test]
  fn remove_reports_whether_a_row_was_deleted() {
    let (client, repo) = scripted();
    assert!(!run(repo.remove(TodoId::new(1))).unwrap());
    client.script_execute(PgTodoRepository::DELETE, 1);
    assert!(run(repo.remove(TodoId::new(1))).unwrap());
  }

  #[test]
  fn a_disconnected_store_is_unavailable() {
    let repo = PgTodoRepository::new(SqlClientService::new(TestSqlClient::new()));
    assert!(matches!(run(repo.all()), Err(StoreError::Unavailable(_))));
    assert!(matches!(
      run(repo.add_all(vec![title("a")])),
      Err(StoreError::Unavailable(_))
    ));
  }
}

/// Live PostgreSQL tests (skip-or-fail protocol: [`LivePg`](super::LivePg)).
#[cfg(test)]
mod pg_tests {
  use id_effect::{Cause, Exit};

  use super::*;
  use crate::infrastructure::postgres::LivePg;

  async fn run<A: Send + 'static>(eff: Effect<A, StoreError>) -> A {
    match LivePg::exit(eff).await {
      Exit::Success(a) => a,
      Exit::Failure(Cause::Fail(e)) => panic!("store error: {e}"),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  async fn live_repo(test: &str) -> Option<(PgTodoRepository, LivePg)> {
    let pg = LivePg::connect(test).await?;
    Some((PgTodoRepository::new(pg.sql.clone()), pg))
  }

  async fn titles(repo: &PgTodoRepository) -> Vec<String> {
    run(repo.all())
      .await
      .into_iter()
      .map(|t| t.title().as_str().to_owned())
      .collect()
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn live_postgres_crud_round_trip() {
    let Some((repo, pg)) = live_repo("live_postgres_crud_round_trip").await else {
      return;
    };
    let (a, b) = (
      LivePg::unique_title("crud-a"),
      LivePg::unique_title("crud-b"),
    );
    let todo = run(repo.add(a.clone())).await.unwrap();
    assert_eq!((todo.title(), todo.is_completed()), (&a, false));
    assert_eq!(run(repo.find(todo.id())).await, Some(todo.clone()));
    assert_eq!(run(repo.add(a.clone())).await, Err(TitleTaken(a.clone())));
    let other = run(repo.add(b.clone())).await.unwrap();
    let mut renamed = other.clone();
    renamed.rename(a.clone());
    assert_eq!(
      run(repo.save(renamed)).await,
      Err(Or::Right(TitleTaken(a.clone())))
    );
    let mut done = todo.clone();
    done.complete();
    assert_eq!(run(repo.save(done.clone())).await, Ok(done.clone()));
    assert_eq!(run(repo.find(todo.id())).await, Some(done.clone()));
    assert!(titles(&repo).await.contains(&a.as_str().to_owned()));
    assert!(run(repo.remove(todo.id())).await);
    assert!(run(repo.remove(other.id())).await);
    assert_eq!(run(repo.find(todo.id())).await, None);
    assert!(!run(repo.remove(todo.id())).await);
    assert_eq!(
      run(repo.save(done)).await,
      Err(Or::Left(TodoNotFound(todo.id())))
    );
    pg.close().await;
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn live_postgres_a_failed_batch_rolls_back() {
    let Some((repo, pg)) = live_repo("live_postgres_a_failed_batch_rolls_back").await else {
      return;
    };
    let (a, b, c) = (
      LivePg::unique_title("tx-a"),
      LivePg::unique_title("tx-b"),
      LivePg::unique_title("tx-c"),
    );
    let kept = run(repo.add(a.clone())).await.unwrap();
    // `b` is inserted first, then `a` is taken: the transaction rolls `b` back.
    assert_eq!(
      run(repo.add_all(vec![b.clone(), a.clone()])).await,
      Err(TitleTaken(a.clone()))
    );
    let now = titles(&repo).await;
    assert!(!now.contains(&b.as_str().to_owned()), "b was rolled back");
    assert!(now.contains(&a.as_str().to_owned()));
    // A title repeated inside the batch rolls back too.
    assert_eq!(
      run(repo.add_all(vec![c.clone(), c.clone()])).await,
      Err(TitleTaken(c.clone()))
    );
    assert!(!titles(&repo).await.contains(&c.as_str().to_owned()));
    // A clean batch commits.
    let made = run(repo.add_all(vec![b.clone(), c.clone()])).await.unwrap();
    let now = titles(&repo).await;
    assert!(now.contains(&b.as_str().to_owned()) && now.contains(&c.as_str().to_owned()));
    for id in made.iter().map(Todo::id).chain([kept.id()]) {
      assert!(run(repo.remove(id)).await);
    }
    pg.close().await;
  }
}
