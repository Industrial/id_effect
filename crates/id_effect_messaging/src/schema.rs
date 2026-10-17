//! The crate's PostgreSQL schema: the tables `id_effect_outbox` and `id_effect_inbox`, shipped as
//! `migrations/0001_id_effect_messaging.{up,down}.sql` and as the [`migrator`].

use id_effect_sql::migrate::Migrator;

/// The crate's migration `migrations/0001_id_effect_messaging.up.sql`.
pub(crate) const SCHEMA: &str = include_str!("../migrations/0001_id_effect_messaging.up.sql");
/// Its down script.
const SCHEMA_DOWN: &str = include_str!("../migrations/0001_id_effect_messaging.down.sql");

/// The tracking table of [`migrator`]: separate from the application's (and from
/// `id_effect_jobs`'), so the crate's version numbers never collide with other migrations.
pub const MIGRATIONS_TABLE: &str = "_id_effect_messaging_migrations";

/// The crate's schema (the outbox and inbox tables) as an `id_effect_sql` [`Migrator`] tracked in
/// its own table [`MIGRATIONS_TABLE`], so the version numbers never collide with the
/// application's migrations. Run it at startup (`migrator().run_on(client)` or
/// `migrator().layer()`); it takes the migration advisory lock, so replicas starting together
/// apply it once. The alternative is to copy `migrations/0001_id_effect_messaging.{up,down}.sql`
/// into the application's own migrations.
pub fn migrator() -> Migrator {
  Migrator::from_files([
    ("0001_id_effect_messaging.up.sql", SCHEMA),
    ("0001_id_effect_messaging.down.sql", SCHEMA_DOWN),
  ])
  .expect("the embedded id_effect_messaging migrations are valid")
  .with_table(MIGRATIONS_TABLE)
}

/// The SQL statements of a migration file: `--` comment lines dropped, split at `;`.
pub(crate) fn statements(script: &str) -> Vec<String> {
  let code: String = script
    .lines()
    .filter(|line| !line.trim_start().starts_with("--"))
    .map(|line| format!("{line}\n"))
    .collect();
  code
    .split(';')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_migrator_has_its_own_tracking_table() {
    let m = migrator();
    assert_eq!(m.table(), MIGRATIONS_TABLE);
    assert_eq!(m.migrations().len(), 1);
    let first = &m.migrations()[0];
    assert_eq!((first.version(), first.is_reversible()), (1, true));
    assert_eq!(first.sql(), SCHEMA);
  }

  #[test]
  fn statements_drop_comments_and_split() {
    let sql = statements(SCHEMA);
    assert_eq!(sql.len(), 5, "{sql:?}");
    assert!(sql[0].starts_with("CREATE TABLE IF NOT EXISTS id_effect_outbox"));
    assert!(sql[1].contains("id_effect_outbox_pending_idx"));
    assert!(sql[2].contains("id_effect_outbox_sent_idx"));
    assert!(sql[3].starts_with("CREATE TABLE IF NOT EXISTS id_effect_inbox"));
    assert!(sql[4].contains("id_effect_inbox_processed_idx"));
    assert!(sql.iter().all(|s| !s.contains("--") && !s.ends_with(';')));
  }
}
