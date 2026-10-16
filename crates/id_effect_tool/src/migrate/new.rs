//! `id-effect migrate new`: the next migration's version and file names, and writing them.
//!
//! Numbering ([`Numbering::Infer`], the default) follows the directory: no migration yet, or
//! timestamped ones (14 digits and more), gets the UTC time `YYYYMMDDHHMMSS` as sqlx-cli does;
//! short sequential versions (the `service` template's `0001_init.sql`) continue the sequence at
//! the widest existing width (`0002`). A timestamp that is not later than the newest version
//! becomes that version + 1, so versions only grow.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::Failure;

/// Digits of a `YYYYMMDDHHMMSS` version; shorter versions are sequential.
const TIMESTAMP_DIGITS: usize = 14;

/// Width of the first sequential version (`0001`, as sqlx-cli writes it).
const SEQUENTIAL_WIDTH: usize = 4;

/// How `migrate new` numbers a migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Numbering {
  /// Follow the directory (see the [module docs](self)).
  Infer,
  /// The UTC time, `YYYYMMDDHHMMSS` (`--timestamp`).
  Timestamp,
  /// The newest version + 1 (`--sequential`).
  Sequential,
}

/// The migration name from the command line: ASCII letters, digits, `_`, `-` and spaces, the
/// last two written as `_` (`add users` → `add_users`).
pub(crate) fn parse_name(name: &str) -> Result<String, String> {
  let name = name.trim();
  if name.is_empty() {
    return Err("the migration name is empty".into());
  }
  if let Some(c) = name
    .chars()
    .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' ')))
  {
    return Err(format!(
      "`{c}` is not allowed in a migration name (ASCII letters, digits, `_`, `-`, spaces)"
    ));
  }
  Ok(name.replace([' ', '-'], "_"))
}

/// The `(version, digits)` of every migration file in `dir` (none when it cannot be read: a
/// directory that does not exist yet; [`create`] reports any other problem when it writes).
fn existing(dir: &Path) -> Vec<(i64, usize)> {
  let Ok(entries) = fs::read_dir(dir) else {
    return Vec::new();
  };
  entries
    .filter_map(Result::ok)
    .filter_map(|entry| {
      let name = entry.file_name().to_string_lossy().into_owned();
      let (prefix, rest) = name.split_once('_')?;
      let digits = !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit());
      let version = prefix
        .parse()
        .ok()
        .filter(|_| digits && rest.ends_with(".sql"))?;
      Some((version, prefix.len()))
    })
    .collect()
}

/// The next version after `existing` (`(version, digits)` pairs), as its file name prefix.
pub(crate) fn next_version(
  existing: &[(i64, usize)],
  numbering: Numbering,
  now: jiff::Timestamp,
) -> String {
  let newest = existing.iter().map(|(v, _)| *v).max();
  let sequential = match numbering {
    Numbering::Timestamp => false,
    Numbering::Sequential => true,
    Numbering::Infer => !existing.is_empty() && existing.iter().all(|(_, d)| *d < TIMESTAMP_DIGITS),
  };
  if sequential {
    let width = existing
      .iter()
      .map(|(_, d)| *d)
      .max()
      .unwrap_or(SEQUENTIAL_WIDTH);
    let next = newest.unwrap_or(0).saturating_add(1);
    return format!("{next:0width$}");
  }
  let stamp = now.strftime("%Y%m%d%H%M%S").to_string();
  match (stamp.parse::<i64>(), newest) {
    (Ok(t), Some(n)) if t <= n => n.saturating_add(1).to_string(),
    _ => stamp,
  }
}

/// Writes the new migration's files in `dir` (created when missing): `<VERSION>_<NAME>.sql`, or
/// the `.up.sql` / `.down.sql` pair when `reversible`.
pub(crate) fn create(
  dir: &Path,
  name: &str,
  reversible: bool,
  numbering: Numbering,
  now: jiff::Timestamp,
) -> Result<Vec<PathBuf>, Failure> {
  let version = next_version(&existing(dir), numbering, now);
  let description = name.replace('_', " ");
  let path = |suffix: &str| dir.join(format!("{version}_{name}{suffix}"));
  let files = if reversible {
    vec![
      (
        path(".up.sql"),
        format!("-- {description}: the migration (`id-effect migrate run`, one transaction).\n"),
      ),
      (
        path(".down.sql"),
        format!("-- {description}: undo the up script (`id-effect migrate revert`).\n"),
      ),
    ]
  } else {
    vec![(
      path(".sql"),
      format!("-- {description}: the migration (`id-effect migrate run`, one transaction).\n"),
    )]
  };
  let io = |path: &Path, e: std::io::Error| Failure::Write(format!("{}: {e}", path.display()));
  fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
  // The version is above every existing one, so these names are free; `create_new` still
  // refuses to overwrite.
  for (path, body) in &files {
    OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(path)
      .and_then(|mut f| f.write_all(body.as_bytes()))
      .map_err(|e| io(path, e))?;
  }
  Ok(files.into_iter().map(|(p, _)| p).collect())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn at(stamp: &str) -> jiff::Timestamp {
    stamp.parse().unwrap()
  }

  const NOW: &str = "2026-10-16T21:09:02Z";

  #[test]
  fn names_allow_words_and_normalise_separators() {
    assert_eq!(
      parse_name(" add users-table ").as_deref(),
      Ok("add_users_table")
    );
    assert_eq!(parse_name("init_0001").as_deref(), Ok("init_0001"));
    assert!(parse_name("  ").unwrap_err().contains("empty"));
    assert!(
      parse_name("a.sql")
        .unwrap_err()
        .contains("`.` is not allowed")
    );
    assert!(parse_name("drop/x").is_err());
  }

  #[test]
  fn an_empty_directory_gets_a_utc_timestamp() {
    assert_eq!(
      next_version(&[], Numbering::Infer, at(NOW)),
      "20261016210902"
    );
    assert_eq!(
      next_version(&[], Numbering::Timestamp, at(NOW)),
      "20261016210902"
    );
  }

  #[test]
  fn sequential_directories_continue_at_their_width() {
    let template = [(1, 4)];
    assert_eq!(next_version(&template, Numbering::Infer, at(NOW)), "0002");
    assert_eq!(
      next_version(&[(1, 1), (9, 1)], Numbering::Infer, at(NOW)),
      "10"
    );
    assert_eq!(
      next_version(&[(41, 3), (7, 5)], Numbering::Infer, at(NOW)),
      "00042"
    );
    assert_eq!(next_version(&[], Numbering::Sequential, at(NOW)), "0001");
    assert_eq!(
      next_version(&template, Numbering::Timestamp, at(NOW)),
      "20261016210902"
    );
  }

  #[test]
  fn timestamps_only_grow() {
    let stamped = [(20_261_016_210_902, 14)];
    assert_eq!(
      next_version(&stamped, Numbering::Infer, at(NOW)),
      "20261016210903",
      "same second: newest + 1"
    );
    let later = at("2026-10-16T21:10:00Z");
    assert_eq!(
      next_version(&stamped, Numbering::Infer, later),
      "20261016211000"
    );
    assert_eq!(
      next_version(&stamped, Numbering::Sequential, later),
      "20261016210903"
    );
  }

  #[test]
  fn create_writes_simple_and_reversible_migrations() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("migrations");
    let simple = create(&dir, "init", false, Numbering::Infer, at(NOW)).unwrap();
    assert_eq!(simple, [dir.join("20261016210902_init.sql")]);
    let body = fs::read_to_string(&simple[0]).unwrap();
    assert!(body.starts_with("-- init: the migration"), "{body}");
    let pair = create(&dir, "add_users", true, Numbering::Infer, at(NOW)).unwrap();
    assert_eq!(
      pair,
      [
        dir.join("20261016210903_add_users.up.sql"),
        dir.join("20261016210903_add_users.down.sql")
      ]
    );
    let down = fs::read_to_string(&pair[1]).unwrap();
    assert!(down.starts_with("-- add users: undo"), "{down}");
  }

  #[test]
  fn existing_reads_only_migration_files() {
    let tmp = tempfile::tempdir().unwrap();
    for name in [
      "0001_init.sql",
      "0003_tags.up.sql",
      "0003_tags.down.sql",
      "README.md",
      "v2_x.sql",
      "_x.sql",
      "0002_notes.txt",
    ] {
      fs::write(tmp.path().join(name), "").unwrap();
    }
    let mut found = existing(tmp.path());
    found.sort_unstable();
    assert_eq!(found, [(1, 4), (3, 4), (3, 4)]);
    assert!(existing(&tmp.path().join("nope")).is_empty());
  }

  #[test]
  fn create_continues_the_template_sequence_and_reports_an_unusable_directory() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("0001_init.sql"), "").unwrap();
    let next = create(tmp.path(), "add_users", false, Numbering::Infer, at(NOW)).unwrap();
    assert_eq!(next, [tmp.path().join("0002_add_users.sql")]);
    let file = tmp.path().join("0001_init.sql");
    let Err(Failure::Write(msg)) = create(&file, "x", false, Numbering::Infer, at(NOW)) else {
      panic!("a file is not a directory");
    };
    assert!(msg.contains("0001_init.sql"), "{msg}");
    let Err(Failure::Write(msg)) = create(&file.join("sub"), "x", false, Numbering::Infer, at(NOW))
    else {
      panic!("a directory under a file cannot be created");
    };
    assert!(msg.contains("sub"), "{msg}");
  }
}
