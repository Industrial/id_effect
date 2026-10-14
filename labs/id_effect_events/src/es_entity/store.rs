//! PostgreSQL event journal on the shared pool (es-entity production path).

use crate::error::EventStoreError;
use crate::event_store::{EventStore, StoredEvent};
use crate::sql_journal::JournalRow;
use id_effect::Effect;
use id_effect::kernel::box_future;
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{PgPool, Row};
use std::marker::PhantomData;
use std::sync::Arc;
use uuid::Uuid;

/// DDL for the es-entity stream journal table (apply in migrations or at startup).
pub const ES_ENTITY_EVENT_JOURNAL_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS es_entity_event_journal (
  stream_id TEXT NOT NULL,
  version BIGINT NOT NULL,
  event_id TEXT NOT NULL,
  payload JSONB NOT NULL,
  PRIMARY KEY (stream_id, version)
);
CREATE INDEX IF NOT EXISTS es_entity_event_journal_stream_idx
  ON es_entity_event_journal (stream_id, version);
"#;

fn sqlx_to_io(err: sqlx::Error) -> EventStoreError {
  EventStoreError::Io(err.to_string())
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
  matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

/// Apply [`ES_ENTITY_EVENT_JOURNAL_DDL`] to `pool`.
pub async fn apply_es_entity_journal_ddl(pool: &PgPool) -> Result<(), EventStoreError> {
  for statement in ES_ENTITY_EVENT_JOURNAL_DDL
    .split(';')
    .map(str::trim)
    .filter(|s| !s.is_empty())
  {
    sqlx::query(statement)
      .execute(pool)
      .await
      .map_err(sqlx_to_io)?;
  }
  Ok(())
}

/// Production PostgreSQL journal backend (transactional append on shared pool).
#[derive(Clone)]
pub struct EsEntityPgBackend {
  pool: Arc<PgPool>,
}

impl EsEntityPgBackend {
  /// Wrap a shared sqlx [`PgPool`].
  #[inline]
  pub fn new(pool: PgPool) -> Self {
    Self {
      pool: Arc::new(pool),
    }
  }

  /// Borrow the underlying pool.
  #[inline]
  pub fn pool(&self) -> &PgPool {
    &self.pool
  }

  /// Append multiple rows in one sqlx transaction (same pool as es-entity `DbOp`).
  pub async fn append_rows(
    &self,
    stream_id: &str,
    rows: &[(u64, String, String)],
  ) -> Result<(), EventStoreError> {
    if rows.is_empty() {
      return Ok(());
    }
    let mut tx = self.pool.begin().await.map_err(sqlx_to_io)?;
    for (version, event_id, payload_json) in rows {
      let result = sqlx::query(
        r#"
        INSERT INTO es_entity_event_journal (stream_id, version, event_id, payload)
        VALUES ($1, $2, $3, $4::jsonb)
        "#,
      )
      .bind(stream_id)
      .bind(*version as i64)
      .bind(event_id)
      .bind(payload_json)
      .execute(&mut *tx)
      .await;

      if let Err(err) = result {
        return if is_unique_violation(&err) {
          Err(EventStoreError::VersionConflict {
            stream_id: stream_id.to_owned(),
            expected: version.saturating_sub(1),
            actual: *version,
          })
        } else {
          Err(sqlx_to_io(err))
        };
      }
    }
    tx.commit().await.map_err(sqlx_to_io)
  }

  /// Read journal rows from `from_version` inclusive.
  pub async fn select_from(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Result<Vec<JournalRow>, EventStoreError> {
    let rows = sqlx::query(
      r#"
      SELECT version, event_id, payload::text AS payload
      FROM es_entity_event_journal
      WHERE stream_id = $1 AND version >= $2
      ORDER BY version
      "#,
    )
    .bind(stream_id)
    .bind(from_version as i64)
    .fetch_all(self.pool.as_ref())
    .await
    .map_err(sqlx_to_io)?;

    rows
      .into_iter()
      .map(|row| {
        let version: i64 = row.try_get("version").map_err(sqlx_to_io)?;
        let event_id: String = row.try_get("event_id").map_err(sqlx_to_io)?;
        let payload: String = row.try_get("payload").map_err(sqlx_to_io)?;
        Ok((version as u64, event_id, payload))
      })
      .collect()
  }
}

/// [`EventStore`] backed by [`EsEntityPgBackend`] (async PG I/O).
pub struct EsEntityEventStore<E> {
  backend: EsEntityPgBackend,
  _marker: PhantomData<E>,
}

impl<E: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> EsEntityEventStore<E> {
  /// Wrap `backend` as an [`EventStore`].
  pub fn new(backend: EsEntityPgBackend) -> Self {
    Self {
      backend,
      _marker: PhantomData,
    }
  }

  /// Borrow the underlying backend.
  #[inline]
  pub fn backend(&self) -> &EsEntityPgBackend {
    &self.backend
  }
}

impl<E: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> EventStore<E>
  for EsEntityEventStore<E>
{
  fn append(
    &self,
    stream_id: &str,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let events: Vec<E> = events.to_vec();
    let backend = self.backend.clone();
    Effect::new_async(move |_r| {
      box_future(async move {
        let latest = backend
          .select_from(&stream_id, 1)
          .await?
          .into_iter()
          .map(|(v, _, _)| v)
          .max()
          .unwrap_or(0);

        let mut out = Vec::with_capacity(events.len());
        let mut rows = Vec::with_capacity(events.len());
        for (i, payload) in events.into_iter().enumerate() {
          let version = latest + i as u64 + 1;
          let event_id = Uuid::new_v4().to_string();
          let json =
            serde_json::to_string(&payload).map_err(|e| EventStoreError::Serde(e.to_string()))?;
          rows.push((version, event_id.clone(), json));
          out.push(StoredEvent {
            event_id,
            version,
            payload,
          });
        }
        backend.append_rows(&stream_id, &rows).await?;
        Ok(out)
      })
    })
  }

  fn read(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let backend = self.backend.clone();
    Effect::new_async(move |_r| {
      box_future(async move {
        let rows = backend.select_from(&stream_id, from_version).await?;
        rows
          .into_iter()
          .map(|(version, event_id, json)| {
            let payload: E =
              serde_json::from_str(&json).map_err(|e| EventStoreError::Serde(e.to_string()))?;
            Ok(StoredEvent {
              event_id,
              version,
              payload,
            })
          })
          .collect()
      })
    })
  }

  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let backend = self.backend.clone();
    Effect::new_async(move |_r| {
      box_future(async move {
        Ok(
          backend
            .select_from(&stream_id, 1)
            .await?
            .into_iter()
            .map(|(v, _, _)| v)
            .max()
            .unwrap_or(0),
        )
      })
    })
  }
}
