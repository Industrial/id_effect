//! The conformance suite on SQLite: in memory, in a database file, and in memory behind a
//! `RoutingSqlClient` (reads to two replicas, writes to the primary, all the same database).
//! No server or container is needed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
use id_effect_sql::{RoutingSqlClient, SqlClient, SqlType, SqlValue};

use crate::conformance::{Backend, conformance_suite};

/// A SQLite database (and the file to delete afterwards, if any).
pub struct Sqlite {
  client: Arc<dyn SqlClient>,
  file: Option<PathBuf>,
}

impl Drop for Sqlite {
  fn drop(&mut self) {
    if let Some(file) = &self.file {
      for suffix in ["", "-wal", "-shm"] {
        let mut path = file.clone().into_os_string();
        path.push(suffix);
        let _ = std::fs::remove_file(path);
      }
    }
  }
}

impl Backend for Sqlite {
  fn client(&self) -> Arc<dyn SqlClient> {
    Arc::clone(&self.client)
  }

  /// Uuid, decimal and JSON columns are `TEXT`: SQLite has no such types (and NUMERIC affinity
  /// would turn numeric-looking text into numbers).
  fn column_ddl(&self, t: SqlType) -> &'static str {
    match t {
      SqlType::Bool => "BOOLEAN",
      SqlType::I16 | SqlType::I32 | SqlType::I64 => "INTEGER",
      SqlType::F32 | SqlType::F64 => "REAL",
      SqlType::Bytes => "BLOB",
      SqlType::Timestamp => "TIMESTAMP",
      SqlType::DateTime => "DATETIME",
      SqlType::Date => "DATE",
      SqlType::Time => "TIME",
      _ => "TEXT",
    }
  }

  fn placeholder(&self, n: usize) -> String {
    format!("?{n}")
  }

  fn stored_type(&self, t: SqlType) -> SqlType {
    match t {
      SqlType::I16 | SqlType::I32 => SqlType::I64,
      SqlType::F32 => SqlType::F64,
      SqlType::Uuid | SqlType::Decimal | SqlType::Json => SqlType::Text,
      t => t,
    }
  }

  fn stored_value(&self, value: SqlValue) -> SqlValue {
    match value {
      SqlValue::I16(v) => SqlValue::I64(v.into()),
      SqlValue::I32(v) => SqlValue::I64(v.into()),
      SqlValue::F32(v) => SqlValue::F64(v.into()),
      SqlValue::Uuid(v) => SqlValue::Text(v.hyphenated().to_string()),
      SqlValue::Decimal(v) => SqlValue::Text(v.to_string()),
      SqlValue::Json(v) => SqlValue::Text(v.to_string()),
      v => v,
    }
  }
}

async fn client(config: SqlitePoolConfig) -> SqliteSqlClient {
  let pool = sqlite_pool_from_config(config.with_max_size(4))
    .await
    .expect("sqlite pool");
  SqliteSqlClient::new(pool)
}

/// A fresh in-memory database.
pub async fn memory(_test: &str) -> Option<Sqlite> {
  Some(Sqlite {
    client: Arc::new(client(SqlitePoolConfig::memory()).await),
    file: None,
  })
}

/// A fresh database file under the temp dir, deleted when the test ends.
pub async fn file(test: &str) -> Option<Sqlite> {
  let path = std::env::temp_dir().join(format!(
    "id_effect_sql_{test}_{}.sqlite",
    std::process::id()
  ));
  let _ = std::fs::remove_file(&path);
  // Opening a WAL database file syncs it to disk several times; with the whole workspace's
  // tests running in parallel (CI, coverage) that can exceed the 10 s default, so these tests
  // allow a minute. The defaults themselves are unchanged.
  let mut config = SqlitePoolConfig::file(&path).with_busy_timeout(Duration::from_secs(30));
  config.acquire_timeout = Duration::from_secs(60);
  Some(Sqlite {
    client: Arc::new(client(config).await),
    file: Some(path),
  })
}

/// A fresh in-memory database behind a router whose two replicas are the database itself.
pub async fn routed(_test: &str) -> Option<Sqlite> {
  let db: Arc<dyn SqlClient> = Arc::new(client(SqlitePoolConfig::memory()).await);
  let router = RoutingSqlClient::new(Arc::clone(&db), vec![Arc::clone(&db), db]);
  Some(Sqlite {
    client: Arc::new(router),
    file: None,
  })
}

mod in_memory {
  use super::*;

  conformance_suite!("cf_sqlite_mem", memory);
}

mod in_file {
  use super::*;

  conformance_suite!("cf_sqlite_file", file);
}

mod behind_router {
  use super::*;

  conformance_suite!("cf_sqlite_routed", routed);
}
