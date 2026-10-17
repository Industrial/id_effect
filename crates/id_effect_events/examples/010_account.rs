//! Ex 010 — an event-sourced bank account on SQLite: the aggregate loads from its latest
//! snapshot plus the events after it, and a balances read model is kept by a catch-up
//! subscription that resumes from its stored checkpoint after a restart (the example closes the
//! database file and opens it again).
//!
//! ```bash
//! cargo run -p id_effect_events --example 010_account --features sqlite
//! ```

use std::path::Path;
use std::sync::Arc;

use id_effect::{Effect, Exit};
use id_effect_events::{
  CatchUpSubscription, EventStore, Projection, RecordedEvent, Snapshot, SnapshotStore,
  SqlCheckpointStore, SqlEventJournal, SqlSnapshotStore, load_aggregate,
};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
use id_effect_sql::{SqlClient, SqlError, params};
use id_effect_tokio::TokioRuntime;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum AccountEvent {
  Opened { owner: String },
  Deposited(i64),
  Withdrawn(i64),
}

/// The write model: what a command is checked against.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Account {
  owner: String,
  balance: i64,
}

struct Fold;
impl Projection<Account, AccountEvent> for Fold {
  fn initial(&self) -> Account {
    Account::default()
  }
  fn apply(&self, account: Account, event: &AccountEvent) -> Account {
    match event {
      AccountEvent::Opened { owner } => Account {
        owner: owner.clone(),
        ..account
      },
      AccountEvent::Deposited(n) => Account {
        balance: account.balance + n,
        ..account
      },
      AccountEvent::Withdrawn(n) => Account {
        balance: account.balance - n,
        ..account
      },
    }
  }
}

enum Command {
  Open(&'static str),
  Deposit(i64),
  Withdraw(i64),
}

fn decide(account: &Account, command: &Command) -> Result<Vec<AccountEvent>, &'static str> {
  match command {
    Command::Open(owner) if account.owner.is_empty() => Ok(vec![AccountEvent::Opened {
      owner: (*owner).to_owned(),
    }]),
    Command::Open(_) => Err("already open"),
    Command::Deposit(n) if *n > 0 => Ok(vec![AccountEvent::Deposited(*n)]),
    Command::Withdraw(n) if *n > 0 && *n <= account.balance => {
      Ok(vec![AccountEvent::Withdrawn(*n)])
    }
    Command::Withdraw(_) => Err("insufficient funds"),
    Command::Deposit(_) => Err("amounts must be positive"),
  }
}

/// Snapshot an account once per this many events.
const SNAPSHOT_EVERY: u64 = 3;

/// The read model: balances by account, with the last position applied to each (so a
/// re-delivered event is ignored).
const BALANCES_DDL: &str = "CREATE TABLE IF NOT EXISTS account_balance (
  stream_id TEXT PRIMARY KEY,
  balance BIGINT NOT NULL,
  position BIGINT NOT NULL
)";

const APPLY_SQL: &str = "INSERT INTO account_balance (stream_id, balance, position) \
  VALUES ($1, $2, $3) ON CONFLICT (stream_id) DO UPDATE \
  SET balance = account_balance.balance + excluded.balance, position = excluded.position \
  WHERE excluded.position > account_balance.position";

struct Bank {
  journal: SqlEventJournal<AccountEvent>,
  snapshots: SqlSnapshotStore<Account>,
  balances: CatchUpSubscription<AccountEvent>,
}

async fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(eff: Effect<A, E>) -> A {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => a,
    Exit::Failure(cause) => panic!("example failed: {cause:?}"),
  }
}

/// Open the bank's database (the journal, snapshot, checkpoint and read-model tables).
async fn open(path: &Path) -> Bank {
  let pool = sqlite_pool_from_config(SqlitePoolConfig::file(path))
    .await
    .expect("sqlite");
  let client: Arc<dyn SqlClient> = Arc::new(SqliteSqlClient::new(pool));
  let journal = SqlEventJournal::new(Arc::clone(&client));
  let snapshots = SqlSnapshotStore::new(Arc::clone(&client));
  let checkpoints = SqlCheckpointStore::new(Arc::clone(&client));
  run(journal.create_table()).await;
  run(snapshots.create_table()).await;
  run(checkpoints.create_table()).await;
  run(client.execute(BALANCES_DDL, &[])).await;
  let balances = CatchUpSubscription::new("balances", journal.clone(), checkpoints);
  Bank {
    journal,
    snapshots,
    balances,
  }
}

impl Bank {
  /// Load the account (snapshot + later events), decide, append at the loaded version, and
  /// snapshot when the account crossed a multiple of [`SNAPSHOT_EVERY`].
  async fn execute(&self, id: &str, command: Command) -> Result<Account, &'static str> {
    let loaded = run(load_aggregate(&self.journal, &self.snapshots, id, Fold)).await;
    let events = decide(&loaded.state, &command)?;
    let stored = run(self.journal.append_expected(id, loaded.version, &events)).await;
    let version = stored.last().map_or(loaded.version, |e| e.version);
    let state = events.iter().fold(loaded.state, |a, e| Fold.apply(a, e));
    if version / SNAPSHOT_EVERY > loaded.version / SNAPSHOT_EVERY {
      let snapshot = Snapshot {
        stream_id: id.to_owned(),
        version,
        state: state.clone(),
      };
      run(self.snapshots.save(&snapshot)).await;
      println!("  snapshot {id} at version {version}");
    }
    Ok(state)
  }

  /// Bring the balances read model up to date; the number of events applied.
  async fn project(&self) -> usize {
    let client = Arc::clone(self.journal.client());
    let apply = move |e: &RecordedEvent<AccountEvent>| -> Effect<(), SqlError> {
      let delta = match e.event.payload {
        AccountEvent::Opened { .. } => 0,
        AccountEvent::Deposited(n) => n,
        AccountEvent::Withdrawn(n) => -n,
      };
      let position = e.position as i64;
      client
        .execute(APPLY_SQL, &params![e.stream_id.as_str(), delta, position])
        .map(|_| ())
    };
    run(self.balances.catch_up(apply)).await
  }

  async fn balances(&self) -> Vec<(String, i64)> {
    let rows = run(self.journal.client().query(
      "SELECT stream_id, balance FROM account_balance ORDER BY stream_id",
      &[],
    ))
    .await;
    rows
      .iter()
      .map(|r| {
        (
          r.get::<String>("stream_id").expect("id"),
          r.get::<i64>("balance").expect("balance"),
        )
      })
      .collect()
  }
}

#[tokio::main]
async fn main() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("bank.sqlite");

  println!("first run");
  {
    let bank = open(&path).await;
    for command in [
      Command::Open("alice"),
      Command::Deposit(100),
      Command::Withdraw(30),
      Command::Deposit(50),
    ] {
      bank.execute("account-1", command).await.expect("accepted");
    }
    assert_eq!(
      bank.execute("account-1", Command::Withdraw(500)).await,
      Err("insufficient funds")
    );
    bank
      .execute("account-2", Command::Open("bob"))
      .await
      .expect("open");
    bank
      .execute("account-2", Command::Deposit(20))
      .await
      .expect("deposit");

    let applied = bank.project().await;
    println!("  projected {applied} events: {:?}", bank.balances().await);
    assert_eq!(applied, 6);
  }

  println!("restart");
  let bank = open(&path).await;
  let snapshot = run(bank.snapshots.load("account-1"))
    .await
    .expect("snapshot");
  let account = run(load_aggregate(
    &bank.journal,
    &bank.snapshots,
    "account-1",
    Fold,
  ))
  .await;
  println!(
    "  account-1 = snapshot at version {} + {} later event(s) = {:?}",
    snapshot.version,
    account.version - snapshot.version,
    account.state
  );
  assert_eq!(
    (snapshot.version, account.version, account.state.balance),
    (3, 4, 120)
  );

  bank
    .execute("account-1", Command::Deposit(5))
    .await
    .expect("deposit");
  let checkpoint = run(bank.balances.checkpoint()).await;
  let applied = bank.project().await;
  println!(
    "  resumed after position {checkpoint}: projected {applied} event(s): {:?}",
    bank.balances().await
  );
  assert_eq!(applied, 1);
  assert_eq!(
    bank.balances().await,
    [("account-1".to_owned(), 125), ("account-2".to_owned(), 20)]
  );
}
