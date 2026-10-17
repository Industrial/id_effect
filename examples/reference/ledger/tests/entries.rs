//! Postings: double entry, `SERIALIZABLE` transactions retried on conflicts, a savepoint per
//! batch entry, the hash-chained audit log, and the streamed CSV export.

mod common;

use common::with_ledger;
use id_effect_kit::sql::params;
use id_effect_kit::test::http::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn posting_is_double_entry_and_audited() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let (cash, sales) = (accounts[0].id, accounts[1].id);

      let id = l.transfer(&team.accountant, cash, sales, 2_500).await;
      let entry: Value = l
        .get(&format!("/entries/{id}"), &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(entry["posted_by"], "acme-accountant");
      assert_eq!(entry["lines"].as_array().unwrap().len(), 2);
      let list: Value = l
        .get("/entries?limit=10", &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(list["entries"][0]["id"], id);

      // Unbalanced, a zero line, a single line: rejected by the schema, every issue listed.
      let unbalanced = l
        .post(
          "/entries",
          json!({"memo": "oops", "lines": [
            {"account_id": cash, "amount": 100},
            {"account_id": sales, "amount": -90},
          ]}),
          &team.accountant,
        )
        .await;
      unbalanced.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
      assert!(
        unbalanced.text().contains("must balance"),
        "{}",
        unbalanced.text()
      );
      let malformed = l
        .post(
          "/entries",
          json!({"memo": "", "lines": [{"account_id": cash, "amount": 0}]}),
          &team.accountant,
        )
        .await;
      malformed.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
      let pointers: Vec<String> = malformed.problem().unwrap().extensions["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["pointer"].as_str().unwrap().to_owned())
        .collect();
      assert_eq!(
        pointers,
        ["/memo", "/lines/0/amount"],
        "{}",
        malformed.text()
      );

      // The database enforces double entry too: a deferred constraint trigger refuses, at
      // commit, an entry written behind the application's back that does not balance.
      let refused = l
        .try_execute(
          "WITH e AS (INSERT INTO journal_entries (org_id, memo, posted_by) \
                      VALUES ('acme', 'raw', 'sql') RETURNING id) \
           INSERT INTO postings (org_id, entry_id, account_id, amount) \
           SELECT 'acme', e.id, $1, 1 FROM e",
          params![cash],
        )
        .await;
      let err = refused.expect_err("an unbalanced entry is refused");
      assert!(err.to_string().contains("does not balance"), "{err}");

      // Balances, and the audit record of the posting.
      let balance: Value = l
        .get(&format!("/accounts/{cash}/balance"), &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(balance["balance"], 2_500);
      let audit: Value = l.get("/audit/verify", &team.owner).await.json().unwrap();
      assert_eq!(audit["intact"], true, "{audit}");
      assert_eq!(audit["records"], 1);
    },
  )
  .await;
}

#[tokio::test]
async fn concurrent_postings_all_commit_under_serializable() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 3).await;
      let (cash, sales, fees) = (accounts[0].id, accounts[1].id, accounts[2].id);
      let l = std::sync::Arc::new(l);

      // Sixteen postings racing on the same accounts and the same audit chain: some lose a
      // serialization conflict and are rerun; all of them commit, exactly once.
      let mut tasks = Vec::new();
      for n in 0..16_i64 {
        let (l, who) = (std::sync::Arc::clone(&l), team.accountant.clone());
        let other = if n % 2 == 0 { sales } else { fees };
        tasks.push(tokio::spawn(async move {
          l.transfer(&who, cash, other, 100 + n).await
        }));
      }
      for task in tasks {
        task.await.expect("posting task");
      }
      let trial: Value = l
        .get("/reports/trial-balance", &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(trial["total"], 0, "{trial}");
      let expected: i64 = (0..16).map(|n| 100 + n).sum();
      assert_eq!(trial["accounts"][0]["balance"], expected, "{trial}");
      let audit: Value = l.get("/audit/verify", &team.owner).await.json().unwrap();
      assert_eq!(
        (audit["intact"].clone(), audit["records"].clone()),
        (json!(true), json!(16))
      );
    },
  )
  .await;
}

#[tokio::test]
async fn a_batch_rolls_back_only_the_entry_the_database_refuses() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let (cash, sales) = (accounts[0].id, accounts[1].id);
      let line = |debit: i64, credit: i64, amount: i64| {
        json!({"memo": "batch", "lines": [
          {"account_id": debit, "amount": amount},
          {"account_id": credit, "amount": -amount},
        ]})
      };
      // The middle entry names an account that does not exist: its header is inserted, its
      // posting fails, and only its savepoint is rolled back.
      let res = l
        .post(
          "/entries/batch",
          json!([
            line(cash, sales, 10),
            line(cash, 999_999, 20),
            line(cash, sales, 30)
          ]),
          &team.accountant,
        )
        .await;
      res.assert_status(StatusCode::OK);
      let result: Value = res.json().unwrap();
      let entries = result["entries"].as_array().unwrap();
      assert!(
        entries[0]["entry_id"].is_i64() && entries[2]["entry_id"].is_i64(),
        "{result}"
      );
      assert!(
        entries[1]["entry_id"].is_null() && entries[1]["error"].is_string(),
        "{result}"
      );

      let headers = l
        .scalar("SELECT count(*)::int8 FROM journal_entries", params![])
        .await;
      let postings = l
        .scalar("SELECT count(*)::int8 FROM postings", params![])
        .await;
      assert_eq!((headers, postings), (2, 4));
      let audit: Value = l.get("/audit/verify", &team.owner).await.json().unwrap();
      assert_eq!(
        (audit["intact"].clone(), audit["records"].clone()),
        (json!(true), json!(2))
      );
      let balance: Value = l
        .get(&format!("/accounts/{cash}/balance"), &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(balance["balance"], 40);
    },
  )
  .await;
}

#[tokio::test]
async fn an_edited_audit_record_is_detected() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      l.transfer(&team.accountant, accounts[0].id, accounts[1].id, 70)
        .await;
      l.transfer(&team.accountant, accounts[0].id, accounts[1].id, 80)
        .await;
      let intact: Value = l.get("/audit/verify", &team.owner).await.json().unwrap();
      assert_eq!(intact["intact"], true);

      // Someone with database access rewrites history.
      l.execute(
        "UPDATE audit_log SET actor = 'someone-else' WHERE chain = 'acme' AND seq = 1",
        params![],
      )
      .await;
      let broken: Value = l.get("/audit/verify", &team.owner).await.json().unwrap();
      assert_eq!(broken["intact"], false, "{broken}");
      assert_eq!(broken["records"], 0);
      assert!(
        broken["broken"].as_str().unwrap().contains("hash mismatch"),
        "{broken}"
      );
    },
  )
  .await;
}

#[tokio::test]
async fn the_csv_export_streams_every_posting() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      l.transfer(&team.accountant, accounts[0].id, accounts[1].id, 5)
        .await;
      let res = l
        .post(
          "/entries",
          json!({"memo": "rent, \"march\"", "lines": [
            {"account_id": accounts[1].id, "amount": 7},
            {"account_id": accounts[0].id, "amount": -7},
          ]}),
          &team.accountant,
        )
        .await;
      res.assert_status(StatusCode::CREATED);

      let csv = l.get("/export/postings.csv", &team.viewer).await;
      csv.assert_status(StatusCode::OK);
      assert_eq!(csv.header("content-type"), Some("text/csv; charset=utf-8"));
      let text = csv.text();
      let lines: Vec<&str> = text.lines().collect();
      assert_eq!(lines[0], ledger::reports::CSV_HEADER);
      assert_eq!(lines.len(), 5, "{text}");
      assert!(text.contains("\"rent, \"\"march\"\"\""), "{text}");
    },
  )
  .await;
}
