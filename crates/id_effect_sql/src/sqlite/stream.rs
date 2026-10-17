//! `SqliteSqlClient::query_stream`: rows streamed from a sqlx `fetch` with bounded buffering.
//!
//! The first pull spawns a feeder task on the current tokio runtime. It takes a pooled
//! connection, runs the query with `fetch` and sends the rows in decoded batches of at most
//! [`BATCH_ROWS`] over a channel holding at most [`BUFFERED_BATCHES`]: while the consumer does
//! not pull, the feeder waits and SQLite stops stepping the statement.
//!
//! When the consumer goes away (the stream is dropped, or the fiber pulling it is interrupted),
//! the channel closes. The feeder then drops the fetch, which resets the statement, and returns
//! the connection to the pool.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;

use id_effect::{DEFAULT_CHUNK_SIZE, Stream, from_async};
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteRow;
use tokio::sync::mpsc;

use super::error::sqlite_error;
use super::value::{bind_query, decode_rows};
use crate::{SqlError, SqlRow, SqlValue};

/// Rows per decoded batch the feeder sends.
pub(super) const BATCH_ROWS: usize = DEFAULT_CHUNK_SIZE;
/// Batches the channel holds before the feeder waits for the consumer.
pub(super) const BUFFERED_BATCHES: usize = 2;

type Batch = Result<Vec<SqlRow>, SqlError>;

/// What the first pull needs to start the feeder.
struct Start {
  pool: SqlitePool,
  sql: String,
  params: Vec<SqlValue>,
  tx: mpsc::Sender<Batch>,
}

/// The consumer side of one stream. Dropping it closes the channel, which stops the feeder.
struct Feed {
  start: Mutex<Option<Start>>,
  rx: tokio::sync::Mutex<mpsc::Receiver<Batch>>,
}

/// The rows of `sql`, streamed (see the [module docs](self)).
pub(super) fn query_stream(
  pool: SqlitePool,
  sql: String,
  params: Vec<SqlValue>,
) -> Stream<SqlRow, SqlError> {
  let (tx, rx) = mpsc::channel(BUFFERED_BATCHES);
  let feed = Arc::new(Feed {
    start: Mutex::new(Some(Start {
      pool,
      sql,
      params,
      tx,
    })),
    rx: tokio::sync::Mutex::new(rx),
  });
  Stream::from_pull(move |_| {
    let feed = Arc::clone(&feed);
    from_async(move || async move {
      let start = feed
        .start
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
      if let Some(start) = start {
        spawn(start)?;
      }
      match feed.rx.lock().await.recv().await {
        Some(batch) => batch.map(Some),
        None => Ok(None),
      }
    })
  })
}

/// Spawns the feeder on the current tokio runtime.
fn spawn(start: Start) -> Result<(), SqlError> {
  let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
    SqlError::Unsupported("SqliteSqlClient::query_stream needs a tokio runtime".into())
  })?;
  let Start {
    pool,
    sql,
    params,
    tx,
  } = start;
  runtime.spawn(async move {
    if let Err(e) = feed(&pool, &sql, &params, &tx).await {
      // Nobody may be listening any more; then there is nothing to report.
      let _ = tx.send(Err(e)).await;
    }
  });
  Ok(())
}

/// One poll of the fetch.
enum Pulled {
  /// The consumer went away.
  Closed,
  /// A batch of rows (possibly empty), and whether the result ended after it.
  Rows(bool),
  /// The query failed (after the rows already in the batch).
  Failed(sqlx::Error),
}

async fn feed(
  pool: &SqlitePool,
  sql: &str,
  params: &[SqlValue],
  tx: &mpsc::Sender<Batch>,
) -> Result<(), SqlError> {
  let query = bind_query(sqlx::query(sql), params)?;
  let mut conn = pool.acquire().await.map_err(sqlite_error)?;
  let mut rows = query.fetch(&mut *conn);
  let mut closed = pin!(tx.closed());
  let mut batch: Vec<SqliteRow> = Vec::with_capacity(BATCH_ROWS);
  loop {
    let pulled = poll_fn(|cx| {
      if closed.as_mut().poll(cx).is_ready() {
        return Poll::Ready(Pulled::Closed);
      }
      // Take what is ready, up to a batch; hand over a partial batch rather than wait.
      loop {
        match rows.as_mut().poll_next(cx) {
          Poll::Ready(Some(Ok(row))) => {
            batch.push(row);
            if batch.len() >= BATCH_ROWS {
              return Poll::Ready(Pulled::Rows(false));
            }
          }
          Poll::Ready(Some(Err(e))) => return Poll::Ready(Pulled::Failed(e)),
          Poll::Ready(None) => return Poll::Ready(Pulled::Rows(true)),
          Poll::Pending if batch.is_empty() => return Poll::Pending,
          Poll::Pending => return Poll::Ready(Pulled::Rows(false)),
        }
      }
    })
    .await;
    let (end, failure) = match pulled {
      // Dropping the fetch (on return) resets the statement.
      Pulled::Closed => return Ok(()),
      Pulled::Rows(end) => (end, None),
      Pulled::Failed(e) => (true, Some(sqlite_error(e))),
    };
    if !batch.is_empty() {
      let decoded = decode_rows(&batch)?;
      batch.clear();
      if tx.send(Ok(decoded)).await.is_err() {
        return Ok(());
      }
    }
    if let Some(e) = failure {
      return Err(e);
    }
    if end {
      return Ok(());
    }
  }
}
