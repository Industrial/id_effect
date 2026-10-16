//! `MySqlSqlClient::query_stream`: rows streamed from a sqlx `fetch` with bounded buffering.
//!
//! The first pull spawns a feeder task on the current tokio runtime. It takes a pooled
//! connection, notes its connection id, runs the query with `fetch` and sends the rows in
//! decoded batches of at most [`BATCH_ROWS`] over a channel holding at most
//! [`BUFFERED_BATCHES`]: while the consumer does not pull, the feeder waits, the socket fills and
//! the server stops sending.
//!
//! When the consumer goes away (the stream is dropped, or the fiber pulling it is interrupted),
//! the channel closes. The feeder then stops reading, asks the server to stop the query
//! (`KILL QUERY <id>` from a separate connection, while it still holds the query's connection,
//! so the kill can never hit a later statement on it) and returns the connection to the pool,
//! which reads what is left of the result before reusing it.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;

use id_effect::{DEFAULT_CHUNK_SIZE, Stream, from_async};
use sqlx::mysql::MySqlRow;
use sqlx::{Connection as _, Executor as _, MySqlConnection, MySqlPool, Row as _};
use tokio::sync::mpsc;

use super::error::mysql_error;
use super::value::{bind_query, decode_rows};
use crate::{SqlError, SqlRow, SqlValue};

/// Rows per decoded batch the feeder sends.
pub(super) const BATCH_ROWS: usize = DEFAULT_CHUNK_SIZE;
/// Batches the channel holds before the feeder waits for the consumer.
pub(super) const BUFFERED_BATCHES: usize = 2;

type Batch = Result<Vec<SqlRow>, SqlError>;

/// What the first pull needs to start the feeder.
struct Start {
  pool: MySqlPool,
  sql: String,
  params: Vec<SqlValue>,
  tx: mpsc::Sender<Batch>,
}

/// The consumer side of one stream. Dropping it (the stream is dropped, or a pull waiting on
/// it is interrupted) closes the channel, which stops the feeder and kills the query.
struct Feed {
  start: Mutex<Option<Start>>,
  rx: tokio::sync::Mutex<mpsc::Receiver<Batch>>,
}

/// The rows of `sql`, streamed (see the [module docs](self)).
pub(super) fn query_stream(
  pool: MySqlPool,
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
    SqlError::Unsupported("MySqlSqlClient::query_stream needs a tokio runtime".into())
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

/// How the fetch loop ended.
enum Outcome {
  Drained,
  Abandoned,
  Failed(SqlError),
}

async fn feed(
  pool: &MySqlPool,
  sql: &str,
  params: &[SqlValue],
  tx: &mpsc::Sender<Batch>,
) -> Result<(), SqlError> {
  let query = bind_query(sqlx::query(sql), params)?;
  let mut conn = pool.acquire().await.map_err(mysql_error)?;
  let id: u64 = sqlx::query("SELECT CONNECTION_ID()")
    .fetch_one(&mut *conn)
    .await
    .and_then(|row| row.try_get_unchecked(0))
    .map_err(mysql_error)?;
  let outcome = {
    let mut rows = query.fetch(&mut *conn);
    let mut closed = pin!(tx.closed());
    let mut batch: Vec<MySqlRow> = Vec::with_capacity(BATCH_ROWS);
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
        Pulled::Closed => break Outcome::Abandoned,
        Pulled::Rows(end) => (end, None),
        Pulled::Failed(e) => (true, Some(mysql_error(e))),
      };
      if !batch.is_empty() {
        let decoded = match decode_rows(&batch) {
          Ok(decoded) => decoded,
          Err(e) => break Outcome::Failed(e),
        };
        batch.clear();
        if tx.send(Ok(decoded)).await.is_err() {
          break Outcome::Abandoned;
        }
      }
      if let Some(e) = failure {
        // The server ended the query itself: nothing to kill.
        return Err(e);
      }
      if end {
        break Outcome::Drained;
      }
    }
  };
  match outcome {
    Outcome::Drained => Ok(()),
    Outcome::Abandoned => {
      kill_query(pool, id).await;
      Ok(())
    }
    Outcome::Failed(e) => {
      kill_query(pool, id).await;
      Err(e)
    }
  }
}

/// Asks the server to stop what connection `id` is running, from a fresh connection (the pool
/// may have no other connection to give). Best effort: if it fails, returning the connection to
/// the pool still reads the rest of the result.
async fn kill_query(pool: &MySqlPool, id: u64) {
  let options = pool.connect_options();
  if let Ok(mut conn) = MySqlConnection::connect_with(&options).await {
    let _ = conn.execute(format!("KILL QUERY {id}").as_str()).await;
    let _ = conn.close().await;
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};

  use super::*;
  use crate::mysql::{MySqlPoolConfig, mysql_pool_from_config_lazy};

  #[test]
  fn streaming_outside_a_tokio_runtime_is_unsupported() {
    let rt = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("runtime");
    let pool = rt
      .block_on(async {
        mysql_pool_from_config_lazy(MySqlPoolConfig::from_url("mysql://localhost/none"))
      })
      .expect("lazy pool");
    let rows = query_stream(pool, "SELECT 1".into(), Vec::new())
      .run_collect()
      .block_on();
    assert!(
      matches!(&rows, Exit::Failure(Cause::Fail(SqlError::Unsupported(m))) if m.contains("tokio")),
      "{rows:?}"
    );
  }
}
