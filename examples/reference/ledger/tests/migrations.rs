//! Migrations: the directory `id-effect migrate` and the service share, applied at start through
//! the `migrate` layer — tracked, checksummed, one transaction each under an advisory lock,
//! reversible, and checked.

mod common;

use std::sync::Arc;

use common::{MIGRATIONS, run, with_database};
use id_effect::{Cause, Effect, Exit};
use id_effect_kit::axum::EffectExecutor;
use id_effect_kit::sql::migrate::{MigrateError, MigrationState, Migrator};

/// The typed outcome of `eff`.
async fn outcome<A: Send + 'static>(eff: Effect<A, MigrateError>) -> Result<A, MigrateError> {
  match EffectExecutor::global().run_effect(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(cause) => panic!("{cause:?}"),
  }
}

#[test]
fn the_migrations_directory_is_valid() {
  let migrator = Migrator::from_dir(MIGRATIONS).expect("migrations/");
  let versions: Vec<(i64, bool)> = migrator
    .migrations()
    .iter()
    .map(|m| (m.version(), m.is_reversible()))
    .collect();
  assert_eq!(
    versions,
    [
      (1, true),
      (2, true),
      (3, true),
      (4, true),
      (5, true),
      (6, true),
      (7, true),
      (8, true),
      (9, true)
    ]
  );
}

#[tokio::test]
async fn start_up_migrates_once_and_reverts_on_demand() {
  with_database(|db, _url| async move {
    let migrator = Migrator::from_dir(MIGRATIONS).expect("migrations/");
    let check = || outcome(migrator.check_on(Arc::clone(&db)));
    assert_eq!(
      check().await,
      Err(MigrateError::Pending {
        versions: vec![1, 2, 3, 4, 5, 6, 7, 8, 9]
      })
    );

    // Two instances start together: the advisory lock lets each migration apply once.
    let (a, b) = tokio::join!(
      run(ledger::db::migrate(Arc::clone(&db), MIGRATIONS)),
      run(ledger::db::migrate(Arc::clone(&db), MIGRATIONS)),
    );
    let mut applied = [a, b].concat();
    applied.sort_unstable();
    assert_eq!(applied, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(check().await, Ok(()));
    let states: Vec<MigrationState> = outcome(migrator.status_on(Arc::clone(&db)))
      .await
      .expect("status")
      .into_iter()
      .map(|s| s.state)
      .collect();
    assert_eq!(states, [MigrationState::Applied; 9]);

    // `id-effect migrate revert`: the last migration's down script; check sees it pending.
    assert_eq!(
      outcome(migrator.revert_on(Arc::clone(&db), 1)).await,
      Ok(vec![9])
    );
    assert_eq!(
      check().await,
      Err(MigrateError::Pending { versions: vec![9] })
    );
    // The next start re-applies it, and nothing else.
    assert_eq!(
      run(ledger::db::migrate(Arc::clone(&db), MIGRATIONS)).await,
      [9]
    );
    assert_eq!(check().await, Ok(()));
  })
  .await;
}
