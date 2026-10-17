//! Read replicas: reports and balances read through a `RoutingSqlClient` that sends them to the
//! replica, while writes and the reads that must see them stay on the primary.
//!
//! The "replica" is a second PostgreSQL container with the same schema and the same seed rows,
//! not a streaming replica: the test copies rows into it by hand where replication would. That
//! makes routing observable: a posting the primary has and the replica does not yet have is
//! invisible to the reports.

mod common;

use std::sync::Arc;

use common::{connect, run, with_ledger};
use id_effect::{HNil, succeed};
use id_effect_kit::sql::{SqlClient, SqlError, params, with_transaction};
use id_effect_kit::test::http::StatusCode;
use id_effect_kit::test::{ContainerError, ContainerImage, Postgres};
use ledger::Database;
use serde_json::Value;

/// Copies `table`'s rows with `columns` from `from` to `to` in one transaction (what replication
/// would do; one transaction, so the replica's deferred double-entry check sees whole entries).
async fn replicate(from: &Arc<dyn SqlClient>, to: &Arc<dyn SqlClient>, table: &str, columns: &str) {
  let rows = run(from.query(&format!("SELECT {columns} FROM {table} ORDER BY id"), &[])).await;
  let placeholders: Vec<String> = (1..=columns.split(',').count())
    .map(|n| format!("${n}"))
    .collect();
  let insert = format!(
    "INSERT INTO {table} ({columns}) VALUES ({}) ON CONFLICT DO NOTHING",
    placeholders.join(", ")
  );
  let copy = with_transaction(Arc::clone(to), move |tx| {
    rows.into_iter().fold(succeed(()), |done, row| {
      let insert = tx.execute(&insert, row.values());
      done.flat_map(move |()| insert.map(|_| ()))
    })
  });
  run::<(), SqlError>(copy).await;
}

#[tokio::test]
async fn reports_read_from_the_replica() {
  if !id_effect_kit::test::require_docker() {
    return;
  }
  let container = run(Postgres::new().start::<ContainerError, HNil>()).await;
  let replica = connect(&container.url()).await;
  run(ledger::db::migrate(
    Arc::clone(&replica),
    common::MIGRATIONS,
  ))
  .await;
  let reads = Arc::clone(&replica);
  with_ledger(
    move |_, services| {
      let primary = Arc::clone(&services.database.primary);
      services.database = Database::new(primary, vec![reads]);
    },
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let (cash, sales) = (accounts[0].id, accounts[1].id);
      // The seed reaches the replica.
      replicate(&l.db, &replica, "orgs", "id").await;
      replicate(
        &l.db,
        &replica,
        "accounts",
        "id, org_id, code, name, kind, owner_id, locked, version",
      )
      .await;

      // A posting goes to the primary ...
      let entry = l.transfer(&team.accountant, cash, sales, 900).await;
      l.get(&format!("/entries/{entry}"), &team.viewer)
        .await
        .assert_status(StatusCode::OK);
      // ... and the reports, read on the replica, do not see it yet.
      let trial: Value = l
        .get("/reports/trial-balance", &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(trial["accounts"][0]["balance"], 0, "{trial}");
      let csv = l.get("/export/postings.csv", &team.viewer).await.text();
      assert_eq!(csv.lines().count(), 1, "only the header: {csv}");

      // Once replicated, they do.
      replicate(
        &l.db,
        &replica,
        "journal_entries",
        "id, org_id, memo, posted_by",
      )
      .await;
      replicate(
        &l.db,
        &replica,
        "postings",
        "id, org_id, entry_id, account_id, amount",
      )
      .await;
      let trial: Value = l
        .get("/reports/trial-balance", &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(trial["accounts"][0]["balance"], 900, "{trial}");
      assert_eq!(trial["total"], 0);
      let balance: Value = l
        .get(&format!("/accounts/{sales}/balance"), &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(balance["balance"], -900);
      let replica_rows =
        run(replica.query("SELECT count(*)::int8 AS n FROM postings", &params![])).await;
      assert_eq!(replica_rows[0].get::<i64>("n"), Ok(2));
    },
  )
  .await;
  drop(container);
}
