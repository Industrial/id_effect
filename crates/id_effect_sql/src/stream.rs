//! Streaming query results: [`SqlClient::query_stream`](crate::SqlClient::query_stream) and the
//! environment-reading [`query_stream`] / [`query_stream_as`] return a pull-based
//! [`Stream`] of rows instead of collecting them.
//!
//! - Nothing runs until the stream is pulled, and rows arrive one chunk per pull, so a consumer
//!   that is slower than the database applies backpressure instead of buffering the result.
//! - Dropping the stream (a `take`, a failed consumer) or interrupting the fiber pulling it stops
//!   the query: `PgSqlClient` cancels it on the server and returns its connection to the pool.
//! - `TestSqlClient` serves its scripted rows lazily and counts the streams still open
//!   ([`TestSqlClient::open_streams`](crate::TestSqlClient::open_streams)).
//!
//! ```rust
//! use id_effect::{Exit, Stream};
//! use id_effect_sql::{
//!   SqlClient, SqlClientEnv, SqlError, SqlRow, TestSqlClient, query_stream, sql_client_env,
//! };
//!
//! let db = TestSqlClient::new();
//! let _ = db.connect().block_on();
//! db.script_query(
//!   "SELECT id FROM ledger",
//!   (1..=1000i64).map(|id| SqlRow::from_pairs([("id", id)])).collect(),
//! );
//! let rows: Stream<SqlRow, SqlError, SqlClientEnv> = query_stream("SELECT id FROM ledger", vec![]);
//! let first = rows.take(3).run_collect().provide_all(sql_client_env(db.clone()));
//! let Exit::Success(first) = first.block_on() else { panic!() };
//! assert_eq!(first.len(), 3);
//! assert_eq!(db.open_streams(), 0, "the rest was dropped, not read");
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use id_effect::{Effect, HList, Selector, Stream, fail, need, succeed};

use crate::client::{SqlClientService, TestState, lock};
use crate::error::SqlError;
use crate::from_row::FromRow;
use crate::raw::Sql;
use crate::row::SqlRow;
use crate::value::SqlValue;

/// [`SqlClient::query_stream`](crate::SqlClient::query_stream) on the [`SqlClientService`] in
/// `R`: the client is read on the first pull.
pub fn query_stream<R, I>(sql: impl Into<Sql>, params: Vec<SqlValue>) -> Stream<SqlRow, SqlError, R>
where
  R: Selector<SqlClientService, I> + HList + Send + Sync + 'static,
{
  type Rest = Arc<Mutex<Option<Stream<SqlRow, SqlError>>>>;
  let mut start = Some((sql.into(), params));
  let rest: Rest = Arc::default();
  Stream::from_pull(move |n| {
    let slot = Arc::clone(&rest);
    let current: Effect<Stream<SqlRow, SqlError>, SqlError, R> = match start.take() {
      Some((sql, params)) => {
        need::<SqlClientService>().map(move |c| c.query_stream(sql.as_str(), &params))
      }
      // `from_pull` stops pulling once the inner stream ends, so the rest is always there.
      None => succeed(
        slot
          .lock()
          .unwrap_or_else(PoisonError::into_inner)
          .take()
          .unwrap_or_else(Stream::empty),
      ),
    };
    current.flat_map(move |stream| {
      stream.next_chunk_with_size(n).narrow().map(move |step| {
        step.map(|(chunk, more)| {
          *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(more);
          chunk.into_vec()
        })
      })
    })
  })
}

/// [`query_stream`] decoding every row as `T` ([`FromRow`]); a row that does not decode fails
/// the stream with its error.
pub fn query_stream_as<T, R, I>(
  sql: impl Into<Sql>,
  params: Vec<SqlValue>,
) -> Stream<T, SqlError, R>
where
  T: FromRow + Send + 'static,
  R: Selector<SqlClientService, I> + HList + Send + Sync + 'static,
{
  query_stream::<R, I>(sql, params).map_effect(|row| match T::from_row(&row) {
    Ok(t) => succeed(t),
    Err(e) => fail(e),
  })
}

/// Decrements [`TestSqlClient::open_streams`](crate::TestSqlClient::open_streams) when the
/// stream ends or is dropped.
struct OpenStream(TestState);

impl Drop for OpenStream {
  fn drop(&mut self) {
    let mut state = lock(&self.0);
    state.open_streams = state.open_streams.saturating_sub(1);
  }
}

/// The rows left to serve and the open-stream count they hold.
struct Feed {
  rows: VecDeque<SqlRow>,
  _open: OpenStream,
}

/// `TestSqlClient::query_stream`: on the first pull the query runs against the scripted state
/// (failing like [`SqlClient::query`](crate::SqlClient::query)) and is recorded; the rows are
/// then served at most `n` per pull.
pub(crate) fn test_query_stream(
  state: TestState,
  sql: String,
  params: Vec<SqlValue>,
) -> Stream<SqlRow, SqlError> {
  let mut start = Some((sql, params));
  let mut feed: Option<Feed> = None;
  Stream::from_pull(move |n| {
    if let Some((sql, params)) = start.take() {
      let mut s = lock(&state);
      match s.run_query(&sql) {
        Err(e) => return fail(e),
        Ok(rows) => {
          s.queries.push((sql, params));
          s.open_streams += 1;
          feed = Some(Feed {
            rows: rows.into(),
            _open: OpenStream(Arc::clone(&state)),
          });
        }
      }
    }
    let Some(f) = feed.as_mut().filter(|f| !f.rows.is_empty()) else {
      feed = None;
      return succeed(None);
    };
    let batch: Vec<SqlRow> = f.rows.drain(..n.min(f.rows.len())).collect();
    if f.rows.is_empty() {
      feed = None;
    }
    succeed(Some(batch))
  })
}
