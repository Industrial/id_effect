//! Migration sources: a directory, `(file name, script)` pairs, or a sqlx `Migrator` (what
//! `sqlx::migrate!` embeds), all resolved with sqlx's rules.
//!
//! A file is a migration when its name is `<VERSION>_<DESCRIPTION>.sql` (simple, not
//! reversible) or `<VERSION>_<DESCRIPTION>.up.sql` / `.down.sql` (a reversible pair);
//! `<VERSION>` is an `i64` (`0001`, `20260101120000`). Other files are ignored. The checksum is
//! sqlx's: SHA-384 of the up script.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use sqlx::migrate::{Migration as SqlxMigration, MigrationType};

use super::{MigrateError, Migration};

/// The migrations in `dir` (see the [module docs](self)).
pub(super) fn from_dir(dir: &Path) -> Result<Vec<Migration>, MigrateError> {
  let unreadable = |e: std::io::Error| {
    MigrateError::Source(format!(
      "cannot read migration directory {}: {e}",
      dir.display()
    ))
  };
  let mut files = Vec::new();
  for entry in fs::read_dir(dir).map_err(unreadable)? {
    let entry = entry.map_err(unreadable)?;
    let (path, name) = (
      entry.path(),
      entry.file_name().to_string_lossy().into_owned(),
    );
    if !path.is_file() || parts(&name).is_none() {
      continue;
    }
    let sql = fs::read_to_string(&path).map_err(|e| {
      MigrateError::Source(format!("cannot read migration {}: {e}", path.display()))
    })?;
    files.push((name, sql));
  }
  from_files(files)
}

/// The migrations among `(file name, script)` pairs (see the [module docs](self)).
pub(super) fn from_files(
  files: impl IntoIterator<Item = (String, String)>,
) -> Result<Vec<Migration>, MigrateError> {
  let mut parsed = Vec::new();
  for (name, sql) in files {
    let Some((version, rest)) = parts(&name) else {
      continue;
    };
    let version: i64 = version.parse().map_err(|_| {
      MigrateError::Source(format!(
        "migration file `{name}`: expected an integer version prefix (e.g. `0001_init.sql`)"
      ))
    })?;
    let kind = MigrationType::from_filename(rest);
    let description = rest.trim_end_matches(kind.suffix()).replace('_', " ");
    let no_tx = sql.starts_with("-- no-transaction");
    let migration = SqlxMigration::new(version, description.into(), kind, sql.into(), no_tx);
    parsed.push((migration, name));
  }
  assemble(parsed)
}

/// The migrations of a sqlx `Migrator` (file names rebuilt as `<VERSION>_<DESCRIPTION>.sql`).
pub(super) fn from_sqlx(
  migrator: &sqlx::migrate::Migrator,
) -> Result<Vec<Migration>, MigrateError> {
  assemble(
    migrator
      .iter()
      .map(|m| {
        let name = format!(
          "{}_{}{}",
          m.version,
          m.description.replace(' ', "_"),
          m.migration_type.suffix()
        );
        (m.clone(), name)
      })
      .collect(),
  )
}

/// `(version, rest)` of a migration file name, or `None` for a file that is not one.
fn parts(name: &str) -> Option<(&str, &str)> {
  name
    .split_once('_')
    .filter(|(_, rest)| rest.ends_with(".sql"))
}

/// One version's up and down scripts, each with its file name.
type Pair = (
  Option<(SqlxMigration, String)>,
  Option<(SqlxMigration, String)>,
);

/// Pairs up and down scripts by version, ascending.
fn assemble(parsed: Vec<(SqlxMigration, String)>) -> Result<Vec<Migration>, MigrateError> {
  let mut versions: BTreeMap<i64, Pair> = BTreeMap::new();
  for (migration, file) in parsed {
    if migration.no_tx {
      return Err(MigrateError::Source(format!(
        "migration {file}: `-- no-transaction` is not supported (every migration runs in one \
         transaction)"
      )));
    }
    let pair = versions.entry(migration.version).or_default();
    let slot = if migration.migration_type.is_down_migration() {
      &mut pair.1
    } else {
      &mut pair.0
    };
    if let Some((_, other)) = slot {
      return Err(MigrateError::Source(format!(
        "migrations {other} and {file} have the same version {}",
        migration.version
      )));
    }
    *slot = Some((migration, file));
  }
  versions
    .into_iter()
    .map(|(version, pair)| match pair {
      (Some((up, file)), down) => Ok(Migration {
        version,
        description: up.description.into_owned(),
        file,
        sql: up.sql.into_owned(),
        checksum: up.checksum.into_owned(),
        down: down.map(|(down, file)| (file, down.sql.into_owned())),
      }),
      (None, down) => Err(MigrateError::Source(format!(
        "down migration {} has no up migration",
        down.map(|(_, file)| file).unwrap_or_default()
      ))),
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use std::borrow::Cow;

  use sqlx::migrate::{Migration as SqlxMigration, MigrationType, Migrator as SqlxMigrator};

  use super::{from_dir, from_files, from_sqlx};
  use crate::migrate::MigrateError;

  fn files(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list
      .iter()
      .map(|(n, s)| ((*n).to_owned(), (*s).to_owned()))
      .collect()
  }

  fn source_error(r: Result<Vec<crate::migrate::Migration>, MigrateError>) -> String {
    match r {
      Err(MigrateError::Source(msg)) => msg,
      other => panic!("expected a source error, got {other:?}"),
    }
  }

  #[test]
  fn pairs_up_and_down_and_ignores_other_files() {
    let ms = from_files(files(&[
      ("0002_add_tags.up.sql", "ALTER TABLE t ADD tags TEXT"),
      ("README.md", "not a migration"),
      ("0001_init.sql", "CREATE TABLE t (id INT)"),
      ("0002_add_tags.down.sql", "ALTER TABLE t DROP tags"),
      ("notes.sql", "no version separator"),
    ]))
    .unwrap();
    assert_eq!(ms.len(), 2);
    assert_eq!(
      (ms[0].version(), ms[0].description(), ms[0].file()),
      (1, "init", "0001_init.sql")
    );
    assert!(!ms[0].is_reversible());
    assert_eq!((ms[0].down_file(), ms[0].down_sql()), (None, None));
    assert_eq!(ms[1].description(), "add tags");
    assert_eq!(ms[1].sql(), "ALTER TABLE t ADD tags TEXT");
    assert_eq!(ms[1].down_file(), Some("0002_add_tags.down.sql"));
    assert_eq!(ms[1].down_sql(), Some("ALTER TABLE t DROP tags"));
    assert_eq!(ms[1].checksum().len(), 48, "SHA-384");
    assert_eq!(ms[1].checksum_hex().len(), 96);
  }

  #[test]
  fn rejects_bad_versions_duplicates_orphans_and_no_transaction() {
    assert!(source_error(from_files(files(&[("v1_x.sql", "")]))).contains("integer version"));
    assert!(
      source_error(from_files(files(&[("1_a.sql", ""), ("01_b.up.sql", "")])))
        .contains("same version 1")
    );
    assert!(
      source_error(from_files(files(&[("1_a.down.sql", "")])))
        .contains("1_a.down.sql has no up migration")
    );
    assert!(
      source_error(from_files(files(&[(
        "1_a.sql",
        "-- no-transaction\nCREATE INDEX CONCURRENTLY i ON t (id)"
      )])))
      .contains("no-transaction")
    );
  }

  #[test]
  fn reads_a_directory() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/migrations");
    let ms = from_dir(dir.as_ref()).unwrap();
    let versions: Vec<i64> = ms.iter().map(|m| m.version()).collect();
    assert_eq!(versions, [1, 2, 3]);
    assert!(ms.iter().all(|m| m.is_reversible()));
    assert_eq!(ms[0].file(), "0001_create_widgets.up.sql");
    let missing = source_error(from_dir("/definitely/not/here".as_ref()));
    assert!(missing.contains("/definitely/not/here"), "{missing}");
  }

  #[test]
  fn reads_a_sqlx_migrator_with_the_same_checksums() {
    let up = "CREATE TABLE t (id INT)";
    let migrator = SqlxMigrator {
      migrations: Cow::Owned(vec![
        SqlxMigration::new(
          7,
          "make t".into(),
          MigrationType::ReversibleUp,
          up.into(),
          false,
        ),
        SqlxMigration::new(
          7,
          "make t".into(),
          MigrationType::ReversibleDown,
          "DROP TABLE t".into(),
          false,
        ),
      ]),
      ..SqlxMigrator::DEFAULT
    };
    let ms = from_sqlx(&migrator).unwrap();
    assert_eq!(ms[0].file(), "7_make_t.up.sql");
    assert_eq!(ms[0].down_file(), Some("7_make_t.down.sql"));
    let same = from_files(files(&[("7_make_t.up.sql", up)])).unwrap();
    assert_eq!(ms[0].checksum(), same[0].checksum());
  }
}
