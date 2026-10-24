//! The conformance suite on MySQL 8, plus what only MySQL has: unsigned and `BOOLEAN` ranges,
//! column types without a portable value, `READ ONLY` transactions, deadlocks and lock wait
//! timeouts as serialization failures, and `KILL QUERY` when a stream is dropped.
//!
//! Each test starts its own MySQL container (testcontainers; nextest runs every test in a
//! process of its own), or uses the server at `MYSQL_URL` when that is set. Without Docker the
//! tests print `SKIP` and return, unless `ID_EFFECT_REQUIRE_DOCKER=1` (the CI
//! `containers-integration` job), which turns a missing Docker daemon into a failure.

use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, Exit};
use id_effect_sql::jiff::Timestamp;
use id_effect_sql::jiff::civil::date;
use id_effect_sql::mysql::{
  MySqlPoolConfig, MySqlSqlClient, mysql_pool_from_config, mysql_pool_from_config_lazy,
};
use id_effect_sql::rust_decimal::Decimal;
use id_effect_sql::{
  Isolation, SqlClient, SqlClientEnv, SqlError, SqlType, SqlValue, TransactionOptions,
  is_serialization_failure, params, query as query_eff,
};
use id_effect_tokio::TokioRuntime;
use testcontainers::core::{IntoContainerPort, Mount, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

use crate::conformance::{Backend, conformance_suite, create, drop_table, execute, query, run};

const PASSWORD: &str = "conformance";
const DATABASE: &str = "conformance";

/// A MySQL database under test (and the container it runs in, removed on drop).
pub struct MySql {
  client: MySqlSqlClient,
  url: String,
  container: Option<ContainerAsync<GenericImage>>,
}

impl Drop for MySql {
  /// Removing the container needs a tokio context; the proptest body drops its backend outside
  /// the runtime it used.
  fn drop(&mut self) {
    let Some(container) = self.container.take() else {
      return;
    };
    if tokio::runtime::Handle::try_current().is_ok() {
      drop(container);
    } else if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
    {
      let _entered = rt.enter();
      drop(container);
    }
  }
}

impl Backend for MySql {
  fn client(&self) -> Arc<dyn SqlClient> {
    Arc::new(self.client.clone())
  }

  /// Uuids are `BINARY(16)`; date-times and times keep microseconds (`(6)`).
  fn column_ddl(&self, t: SqlType) -> &'static str {
    match t {
      SqlType::Bool => "BOOLEAN",
      SqlType::I16 => "SMALLINT",
      SqlType::I32 => "INT",
      SqlType::I64 => "BIGINT",
      SqlType::F32 => "FLOAT",
      SqlType::F64 => "DOUBLE",
      SqlType::Bytes => "BLOB",
      SqlType::Uuid => "BINARY(16)",
      SqlType::Timestamp => "TIMESTAMP(6)",
      SqlType::DateTime => "DATETIME(6)",
      SqlType::Date => "DATE",
      SqlType::Time => "TIME(6)",
      SqlType::Decimal => "DECIMAL(38,18)",
      SqlType::Json => "JSON",
      _ => "TEXT",
    }
  }

  fn placeholder(&self, _n: usize) -> String {
    "?".to_owned()
  }

  fn bigint_cast(&self) -> &'static str {
    "SIGNED"
  }

  /// A uuid is stored as 16 bytes.
  fn stored_type(&self, t: SqlType) -> SqlType {
    match t {
      SqlType::Uuid => SqlType::Bytes,
      t => t,
    }
  }

  fn stored_value(&self, value: SqlValue) -> SqlValue {
    match value {
      SqlValue::Uuid(v) => SqlValue::Bytes(v.as_bytes().to_vec()),
      v => v,
    }
  }

  /// `TIMESTAMP` holds 1970-01-01T00:00:01Z to 2038-01-19T03:14:07.999999Z.
  fn stores(&self, value: &SqlValue) -> bool {
    match value {
      SqlValue::Timestamp(t) => (1..2_147_483_648).contains(&t.as_second()),
      _ => true,
    }
  }
}

fn unavailable(test: &str, why: &str) -> Option<MySql> {
  assert!(
    std::env::var("ID_EFFECT_REQUIRE_DOCKER").as_deref() != Ok("1"),
    "{test}: ID_EFFECT_REQUIRE_DOCKER=1 but MySQL is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set MYSQL_URL, or start Docker)");
  None
}

/// MySQL 8 on a tmpfs data directory: `cte_max_recursion_depth` lets the stream bodies count
/// to 100 000; the binary log, performance schema and X protocol are off to start faster.
async fn start_container() -> Result<(ContainerAsync<GenericImage>, String), String> {
  let container = GenericImage::new("mysql", "8.4")
    .with_exposed_port(3306.tcp())
    .with_wait_for(WaitFor::message_on_stderr(
      "port: 3306  MySQL Community Server",
    ))
    .with_env_var("MYSQL_ROOT_PASSWORD", PASSWORD)
    .with_env_var("MYSQL_DATABASE", DATABASE)
    .with_mount(Mount::tmpfs_mount("/var/lib/mysql"))
    .with_cmd([
      "--cte-max-recursion-depth=1000000",
      "--skip-log-bin",
      "--performance-schema=OFF",
      "--mysqlx=OFF",
      "--innodb-buffer-pool-size=32M",
    ])
    .with_startup_timeout(Duration::from_secs(180))
    .start()
    .await
    .map_err(|e| e.to_string())?;
  let host = container.get_host().await.map_err(|e| e.to_string())?;
  let port = container
    .get_host_port_ipv4(3306)
    .await
    .map_err(|e| e.to_string())?;
  let url = format!("mysql://root:{PASSWORD}@{host}:{port}/{DATABASE}");
  Ok((container, url))
}

/// A connected client with a pool of [`Backend::max_connections`], or `None` (skip).
pub async fn live_mysql(test: &str) -> Option<MySql> {
  let (container, url) = match std::env::var("MYSQL_URL") {
    Ok(url) => (None, url),
    Err(_) => match start_container().await {
      Ok((container, url)) => (Some(container), url),
      Err(e) => return unavailable(test, &format!("the MySQL container did not start: {e}")),
    },
  };
  // A dropped stream holds its connection until its query is killed and the connection closed
  // (two round trips); a loaded CI runner gets some slack over the default 5 s acquire timeout.
  let mut config = MySqlPoolConfig::from_url(&url).with_max_size(4);
  config.acquire_timeout = Duration::from_secs(30);
  let pool = match mysql_pool_from_config(config).await {
    Ok(pool) => pool,
    Err(e) => return unavailable(test, &format!("cannot build pool: {}", e.0)),
  };
  Some(MySql {
    client: MySqlSqlClient::new(pool),
    url,
    container,
  })
}

conformance_suite!("cf_mysql", live_mysql);

// ---- MySQL only -------------------------------------------------------------------------------

fn out_of_range(r: Result<SqlValue, SqlError>, column: &str) -> bool {
  matches!(r, Err(SqlError::ValueOutOfRange { column: c, .. }) if c == column)
}

async fn first_value(b: &MySql, sql: &str) -> Result<SqlValue, SqlError> {
  let rows = run(b.client().query(sql, &[])).await?;
  Ok(rows[0].values()[0].clone())
}

#[tokio::test]
async fn mysql_column_types_decode_or_fail_clearly() {
  let t = "my_types";
  let Some(b) = live_mysql(t).await else { return };
  execute(&b, &format!("DROP TABLE IF EXISTS {t}"), &[]).await;
  execute(
    &b,
    &format!(
      "CREATE TABLE {t} (tu TINYINT UNSIGNED, su SMALLINT UNSIGNED, mu MEDIUMINT UNSIGNED, \
       iu INT UNSIGNED, bu BIGINT UNSIGNED, ti TINYINT, mi MEDIUMINT, y YEAR, flag BOOLEAN, \
       e ENUM('a', 'b'), s SET('x', 'y'), ch CHAR(3), vb VARBINARY(4), wide DECIMAL(65,30), \
       tm TIME, bits BIT(8))"
    ),
    &[],
  )
  .await;
  execute(
    &b,
    &format!(
      "INSERT INTO {t} VALUES (255, 65535, 16777215, 4294967295, 9223372036854775807, -128, \
       -8388608, 2024, TRUE, 'b', 'x,y', 'abc', x'00ff', 1.5, '12:34:56', b'101')"
    ),
    &[],
  )
  .await;
  let rows = query(
    &b,
    &format!("SELECT tu, su, mu, iu, bu, ti, mi, y, flag, e, s, ch, vb, wide, tm FROM {t}"),
    &[],
  )
  .await;
  assert_eq!(
    rows[0].values(),
    [
      SqlValue::I16(255),
      SqlValue::I32(65_535),
      SqlValue::I32(16_777_215),
      SqlValue::I64(4_294_967_295),
      SqlValue::I64(i64::MAX),
      SqlValue::I16(-128),
      SqlValue::I32(-8_388_608),
      SqlValue::I16(2024),
      SqlValue::Bool(true),
      SqlValue::Text("b".into()),
      SqlValue::Text("x,y".into()),
      SqlValue::Text("abc".into()),
      SqlValue::Bytes(vec![0, 255]),
      SqlValue::Decimal(Decimal::new(15, 1)),
      SqlValue::Time(date(2000, 1, 1).at(12, 34, 56, 0).time()),
    ]
  );
  let db_types: Vec<&str> = rows[0].columns().iter().map(|c| c.db_type()).collect();
  assert_eq!(
    db_types[..5],
    [
      "TINYINT UNSIGNED",
      "SMALLINT UNSIGNED",
      "MEDIUMINT UNSIGNED",
      "INT UNSIGNED",
      "BIGINT UNSIGNED"
    ]
  );

  assert!(
    query(&b, &format!("SELECT tu FROM {t} WHERE 1 = 0"), &[])
      .await
      .is_empty()
  );

  // Values beyond their portable type are errors naming the column, never a wrapped number.
  let set = |assignment: &str| format!("UPDATE {t} SET {assignment}");
  execute(
    &b,
    &set("bu = 18446744073709551615, flag = 2, tm = '-01:00:00'"),
    &[],
  )
  .await;
  assert!(out_of_range(
    first_value(&b, &format!("SELECT bu FROM {t}")).await,
    "bu"
  ));
  assert!(out_of_range(
    first_value(&b, &format!("SELECT flag FROM {t}")).await,
    "flag"
  ));
  assert!(out_of_range(
    first_value(&b, &format!("SELECT tm FROM {t}")).await,
    "tm"
  ));
  execute(&b, &set("tm = '25:00:00'"), &[]).await;
  assert!(out_of_range(
    first_value(&b, &format!("SELECT tm FROM {t}")).await,
    "tm"
  ));
  // A DECIMAL wider than 28 digits.
  execute(&b, &set("wide = 1234567890123456789012345678901234.5"), &[]).await;
  assert!(out_of_range(
    first_value(&b, &format!("SELECT wide FROM {t}")).await,
    "wide"
  ));
  // BIT has no portable value: cast it.
  let bits = first_value(&b, &format!("SELECT bits FROM {t}")).await;
  assert!(
    matches!(&bits, Err(SqlError::Unsupported(m)) if m.contains("`bits` has type BIT")),
    "{bits:?}"
  );
  assert_eq!(
    first_value(&b, &format!("SELECT CAST(bits AS UNSIGNED) AS n FROM {t}")).await,
    Ok(SqlValue::I64(5))
  );

  // Years before 0000 are refused before anything is sent; a TIMESTAMP column refuses 1969.
  let negative = run(
    b.client()
      .execute(&format!("UPDATE {t} SET y = ?"), &params![date(-1, 1, 1)]),
  )
  .await;
  assert!(matches!(negative, Err(SqlError::ValueOutOfRange { column, .. }) if column == "$1"));
  create(&b, "my_ts", &[("at", SqlType::Timestamp)]).await;
  let early: Timestamp = "1969-07-20T20:17:40Z".parse().unwrap();
  let refused = run(
    b.client()
      .execute("INSERT INTO my_ts (at) VALUES (?)", &params![early]),
  )
  .await;
  assert!(
    matches!(&refused, Err(SqlError::QueryFailed { message, .. }) if message.ends_with("(SQLSTATE 22007)")),
    "{refused:?}"
  );
  drop_table(&b, "my_ts").await;

  // Date-times bind as literals: a bare `SELECT ?` reads back as text, a cast keeps the type.
  let local = date(2024, 2, 29).at(12, 34, 56, 123_456_789);
  let bare = query(&b, "SELECT ? AS v", &params![local]).await;
  assert_eq!(
    bare[0].values(),
    [SqlValue::Text("2024-02-29 12:34:56.123456".into())]
  );
  let cast = query(&b, "SELECT CAST(? AS DATETIME(6)) AS v", &params![local]).await;
  let micros = date(2024, 2, 29).at(12, 34, 56, 123_456_000);
  assert_eq!(cast[0].values(), [SqlValue::DateTime(micros)]);

  // Server errors carry their SQLSTATE: a duplicate key (MySQL's 23000) is 23505, as on
  // PostgreSQL.
  execute(&b, "DROP TABLE IF EXISTS my_unique", &[]).await;
  execute(&b, "CREATE TABLE my_unique (id INT PRIMARY KEY)", &[]).await;
  execute(&b, "INSERT INTO my_unique VALUES (1)", &[]).await;
  let duplicate = run(b.client().execute("INSERT INTO my_unique VALUES (1)", &[])).await;
  assert!(
    matches!(&duplicate, Err(e @ SqlError::QueryFailed { .. }) if e.is_unique_violation()),
    "{duplicate:?}"
  );
  drop_table(&b, "my_unique").await;
  drop_table(&b, t).await;
}

/// The isolation level and read-only flag of the connection's open InnoDB transaction.
async fn innodb_trx(tx: &Arc<dyn id_effect_sql::SqlTransaction>, table: &str) -> (String, i64) {
  run(tx.query(&format!("SELECT COUNT(*) AS n FROM {table}"), &[]))
    .await
    .expect("start the InnoDB transaction");
  // InnoDB refreshes its information_schema view of transactions at most every 100 ms.
  tokio::time::sleep(Duration::from_millis(250)).await;
  let rows = run(tx.query(
    "SELECT trx_isolation_level AS level, trx_is_read_only AS ro \
     FROM information_schema.innodb_trx WHERE trx_mysql_thread_id = CONNECTION_ID()",
    &[],
  ))
  .await
  .expect("innodb_trx");
  (rows[0].get("level").unwrap(), rows[0].get("ro").unwrap())
}

#[tokio::test]
async fn mysql_transactions_isolation_read_only_deadlock_lock_wait() {
  let t = "my_locks";
  let Some(b) = live_mysql(t).await else { return };
  create(&b, t, &[("id", SqlType::I64), ("n", SqlType::I64)]).await;
  execute(&b, &format!("ALTER TABLE {t} ADD PRIMARY KEY (id)"), &[]).await;
  execute(
    &b,
    &format!("INSERT INTO {t} (id, n) VALUES (1, 0), (2, 0)"),
    &[],
  )
  .await;
  let client = b.client();

  // The level is set before the transaction starts; READ ONLY refuses writes (25006).
  let tx = run(client.begin_with(Isolation::Serializable.into()))
    .await
    .expect("begin");
  assert_eq!(innodb_trx(&tx, t).await, ("SERIALIZABLE".to_owned(), 0));
  run(tx.commit()).await.expect("commit");
  let options = TransactionOptions::from(Isolation::ReadCommitted).with_read_only();
  let tx = run(client.begin_with(options))
    .await
    .expect("begin read only");
  assert_eq!(innodb_trx(&tx, t).await, ("READ COMMITTED".to_owned(), 1));
  let write = run(tx.execute(&format!("UPDATE {t} SET n = 1"), &[])).await;
  assert!(
    matches!(&write, Err(SqlError::QueryFailed { message, .. }) if message.ends_with("(SQLSTATE 25006)")),
    "{write:?}"
  );
  run(tx.rollback()).await.expect("rollback");

  // A deadlock: each transaction holds the row the other one asks for; InnoDB fails one (1213).
  let (t1, t2) = (
    run(client.begin()).await.unwrap(),
    run(client.begin()).await.unwrap(),
  );
  let update = |id: i64| format!("UPDATE {t} SET n = n + 1 WHERE id = {id}");
  run(t1.execute(&update(1), &[]))
    .await
    .expect("t1 locks row 1");
  run(t2.execute(&update(2), &[]))
    .await
    .expect("t2 locks row 2");
  let (waiter, sql) = (Arc::clone(&t1), update(2));
  let blocked = tokio::spawn(async move { run(waiter.execute(&sql, &[])).await });
  tokio::time::sleep(Duration::from_millis(300)).await;
  let closing = run(t2.execute(&update(1), &[])).await;
  let blocked = blocked.await.expect("join");
  let failures: Vec<SqlError> = [blocked, closing]
    .into_iter()
    .filter_map(Result::err)
    .collect();
  assert_eq!(failures.len(), 1, "{failures:?}");
  assert!(is_serialization_failure(&failures[0]), "{:?}", failures[0]);
  assert!(failures[0].to_string().contains("1213"), "{}", failures[0]);
  let _ = run(t1.rollback()).await;
  let _ = run(t2.rollback()).await;

  // A lock wait timeout (1205) is retryable too.
  let holder = run(client.begin()).await.unwrap();
  run(holder.execute(&update(1), &[]))
    .await
    .expect("lock row 1");
  let waiter = run(client.begin()).await.unwrap();
  run(waiter.execute("SET SESSION innodb_lock_wait_timeout = 1", &[]))
    .await
    .unwrap();
  let timed_out = run(waiter.execute(&update(1), &[])).await.unwrap_err();
  assert!(is_serialization_failure(&timed_out), "{timed_out:?}");
  assert!(timed_out.to_string().contains("1205"), "{timed_out}");
  let _ = run(waiter.rollback()).await;
  run(holder.rollback()).await.expect("rollback");
  drop_table(&b, t).await;
}

/// Whether a statement containing `marker` runs on any other connection.
async fn running(b: &MySql, marker: &str) -> bool {
  let rows = query(
    b,
    "SELECT COUNT(*) AS n FROM information_schema.PROCESSLIST \
     WHERE INFO LIKE ? AND ID <> CONNECTION_ID()",
    &params![format!("%{marker}%")],
  )
  .await;
  rows[0].get::<i64>("n").unwrap() > 0
}

#[tokio::test]
async fn mysql_dropped_stream_kills_its_query_and_layer_closes_the_pool() {
  let t = "my_stream";
  let Some(b) = live_mysql(t).await else { return };
  let client = b.client();
  // One row a millisecond for 100 s, padded so the server flushes rows early.
  let slow = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 100000) \
              SELECT n AS kill_probe_n, REPEAT('x', 200) AS pad FROM s WHERE SLEEP(0.001) = 0";
  let (first, rest) = run(client.query_stream(slow, &[]).next_chunk())
    .await
    .expect("first chunk")
    .expect("rows");
  assert!(!first.is_empty());
  assert!(running(&b, "kill_probe_n").await, "the query streams");
  drop(rest);
  let mut killed = false;
  for _ in 0..100 {
    if !running(&b, "kill_probe_n").await {
      killed = true;
      break;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  assert!(killed, "KILL QUERY stops a dropped stream's statement");
  assert_eq!(query(&b, "SELECT 1 AS n", &[]).await.len(), 1);

  // A row that fails to decode ends the stream with the error; a bad parameter fails first.
  let point = "SELECT ST_GeomFromText('POINT(1 1)') AS g";
  let geometry = run(client.query_stream(point, &[]).run_collect()).await;
  assert!(
    matches!(&geometry, Err(SqlError::Unsupported(m)) if m.contains("GEOMETRY")),
    "{geometry:?}"
  );
  let bad = run(
    client
      .query_stream("SELECT ? AS d", &params![date(-1, 1, 1)])
      .run_collect(),
  )
  .await;
  assert!(
    matches!(bad, Err(SqlError::ValueOutOfRange { .. })),
    "{bad:?}"
  );

  // The scoped layer opens a pool for the program and closes it after; a lazy pool connects on
  // first use.
  let answer: Effect<i64, SqlError, SqlClientEnv> =
    query_eff("SELECT 41 + 1 AS n", vec![]).map(|rows| rows[0].get::<i64>("n").unwrap());
  let program = answer.provide_layer(MySqlSqlClient::layer::<SqlError>(
    MySqlPoolConfig::from_url(&b.url),
  ));
  let answer: Exit<i64, SqlError> = TokioRuntime::current().unwrap().run(program).await;
  assert!(matches!(answer, Exit::Success(42)), "{answer:?}");
  let lazy =
    MySqlSqlClient::new(mysql_pool_from_config_lazy(MySqlPoolConfig::from_url(&b.url)).unwrap());
  assert_eq!(lazy.pool().size(), 0);
  run(lazy.connect()).await.expect("connect");
  run(lazy.close().map_err(|never| match never {}))
    .await
    .expect("close");
  assert_eq!(run(lazy.connect()).await, Err(SqlError::NotConnected));
}
