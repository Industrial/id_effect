//! What `id-effect migrate` prints on success: text for people, one JSON object per command with
//! `--json`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use id_effect_sql::migrate::{MigrationState, MigrationStatus, Migrator};
use serde_json::{Value, json};

/// `pending`, `applied`, `drifted` or `missing`.
pub(crate) fn state_label(state: MigrationState) -> &'static str {
  match state {
    MigrationState::Pending => "pending",
    MigrationState::Applied => "applied",
    MigrationState::Drifted => "drifted",
    MigrationState::Missing => "missing",
  }
}

/// One JSON object and a newline.
fn line(value: &Value) -> String {
  format!("{value}\n")
}

/// The up script of `version` (its version when the source does not have it).
fn up_file(m: &Migrator, version: i64) -> String {
  m.migrations()
    .iter()
    .find(|x| x.version() == version)
    .map_or_else(|| version.to_string(), |x| x.file().to_string())
}

/// The down script of `version` (its version when the source does not have one).
fn down_file(m: &Migrator, version: i64) -> String {
  m.migrations()
    .iter()
    .find(|x| x.version() == version)
    .and_then(|x| x.down_file())
    .map_or_else(|| version.to_string(), str::to_string)
}

/// `migrate new`: the files written.
pub(crate) fn created(paths: &[PathBuf]) -> String {
  paths
    .iter()
    .map(|p| format!("Created {}\n", p.display()))
    .collect()
}

/// `migrate run`: the migrations applied (`{"ok": true, "applied": [versions]}`).
pub(crate) fn applied(m: &Migrator, versions: &[i64], json: bool) -> String {
  if json {
    return line(&json!({ "ok": true, "applied": versions }));
  }
  if versions.is_empty() {
    return "Up to date: no pending migrations.\n".into();
  }
  let mut out = String::new();
  for v in versions {
    let _ = writeln!(out, "applied  {}", up_file(m, *v));
  }
  let _ = writeln!(out, "\nApplied {}.", count(versions.len()));
  out
}

/// `migrate revert`: the migrations reverted, newest first
/// (`{"ok": true, "reverted": [versions]}`).
pub(crate) fn reverted(m: &Migrator, versions: &[i64], json: bool) -> String {
  if json {
    return line(&json!({ "ok": true, "reverted": versions }));
  }
  if versions.is_empty() {
    return "Nothing to revert: no migration is applied.\n".into();
  }
  let mut out = String::new();
  for v in versions {
    let _ = writeln!(out, "reverted  {}", down_file(m, *v));
  }
  let _ = writeln!(out, "\nReverted {}.", count(versions.len()));
  out
}

/// `migrate check` passed (`{"ok": true, "migrations": n}`).
pub(crate) fn checked(m: &Migrator, json: bool) -> String {
  let n = m.migrations().len();
  if json {
    return line(&json!({ "ok": true, "migrations": n }));
  }
  format!(
    "Up to date: {} applied, none pending, changed or missing.\n",
    count(n)
  )
}

/// `migrate status`: a line per migration and the counts, or
/// `{"ok", "dir", "table", "counts": {state: n}, "migrations": [{"version", "description",
/// "state", "reversible", "checksum", "applied_checksum", "applied_at"}]}` (`ok`: every
/// migration applied unchanged).
pub(crate) fn status(dir: &Path, m: &Migrator, statuses: &[MigrationStatus], json: bool) -> String {
  let states = [
    MigrationState::Applied,
    MigrationState::Pending,
    MigrationState::Drifted,
    MigrationState::Missing,
  ];
  let tally = |state| statuses.iter().filter(|s| s.state == state).count();
  if json {
    let counts: serde_json::Map<String, Value> = states
      .iter()
      .map(|s| (state_label(*s).to_string(), json!(tally(*s))))
      .collect();
    let migrations: Vec<Value> = statuses
      .iter()
      .map(|s| {
        json!({
          "version": s.version,
          "description": s.description,
          "state": state_label(s.state),
          "reversible": s.reversible,
          "checksum": s.checksum_hex(),
          "applied_checksum": s.applied_checksum.as_deref().map(hex),
          "applied_at": s.applied_at.map(|t| t.to_string()),
        })
      })
      .collect();
    return line(&json!({
      "ok": statuses.iter().all(|s| s.state == MigrationState::Applied),
      "dir": dir.display().to_string(),
      "table": m.table(),
      "counts": counts,
      "migrations": migrations,
    }));
  }
  let mut out = format!(
    "id-effect migrate status: {} (table {})\n",
    dir.display(),
    m.table()
  );
  let width = statuses
    .iter()
    .map(|s| s.version.to_string().len())
    .max()
    .unwrap_or(0);
  for s in statuses {
    let at = s.applied_at.map_or_else(String::new, |t| {
      t.strftime("%Y-%m-%d %H:%M:%SZ").to_string()
    });
    let reversible = if s.reversible { "  (reversible)" } else { "" };
    let _ = writeln!(
      out,
      "  {:7}  {:>width$}  {at:20}  {}{reversible}",
      state_label(s.state),
      s.version,
      s.description,
    );
  }
  if statuses.is_empty() {
    out.push_str("  (no migrations)\n");
  }
  let counts: Vec<String> = states
    .iter()
    .map(|s| format!("{} {}", tally(*s), state_label(*s)))
    .collect();
  let _ = writeln!(out, "\n{}", counts.join(", "));
  out
}

/// `1 migration`, `3 migrations`.
fn count(n: usize) -> String {
  format!("{n} migration{}", if n == 1 { "" } else { "s" })
}

fn hex(bytes: &[u8]) -> String {
  bytes.iter().fold(String::new(), |mut s, b| {
    let _ = write!(s, "{b:02x}");
    s
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn migrator() -> Migrator {
    Migrator::from_files([
      ("0001_init.sql", "CREATE TABLE a (id INT)"),
      ("0002_tags.up.sql", "CREATE TABLE b (id INT)"),
      ("0002_tags.down.sql", "DROP TABLE b"),
    ])
    .unwrap()
  }

  /// One JSON object on one line.
  fn parse(text: &str) -> Value {
    let (object, rest) = text.split_once('\n').expect("a line");
    assert!(rest.is_empty(), "one line: {text}");
    serde_json::from_str(object).expect("JSON")
  }

  fn status_of(version: i64, state: MigrationState) -> MigrationStatus {
    let applied = state.is_applied();
    MigrationStatus {
      version,
      description: format!("m {version}"),
      state,
      checksum: vec![0xab, 0x01],
      applied_checksum: applied.then(|| vec![0xcd]),
      applied_at: applied.then(|| "2026-10-16T21:09:02.5Z".parse().unwrap()),
      reversible: version == 2,
    }
  }

  #[test]
  fn run_and_revert_name_the_scripts() {
    let m = migrator();
    assert_eq!(
      applied(&m, &[1, 2], false),
      "applied  0001_init.sql\napplied  0002_tags.up.sql\n\nApplied 2 migrations.\n"
    );
    assert_eq!(
      applied(&m, &[], false),
      "Up to date: no pending migrations.\n"
    );
    assert_eq!(
      parse(&applied(&m, &[1], true)),
      json!({"ok": true, "applied": [1]})
    );
    assert_eq!(
      reverted(&m, &[2, 1], false),
      "reverted  0002_tags.down.sql\nreverted  1\n\nReverted 2 migrations.\n"
    );
    assert_eq!(
      reverted(&m, &[], false),
      "Nothing to revert: no migration is applied.\n"
    );
    assert_eq!(
      parse(&reverted(&m, &[2], true)),
      json!({"ok": true, "reverted": [2]})
    );
    assert_eq!(up_file(&m, 9), "9");
  }

  #[test]
  fn check_and_new_summaries() {
    let m = migrator();
    assert_eq!(
      checked(&m, false),
      "Up to date: 2 migrations applied, none pending, changed or missing.\n"
    );
    assert_eq!(
      parse(&checked(&m, true)),
      json!({"ok": true, "migrations": 2})
    );
    let paths = [
      PathBuf::from("m/1_a.up.sql"),
      PathBuf::from("m/1_a.down.sql"),
    ];
    assert_eq!(
      created(&paths),
      "Created m/1_a.up.sql\nCreated m/1_a.down.sql\n"
    );
    assert_eq!(count(1), "1 migration");
  }

  #[test]
  fn status_text_has_a_line_per_migration_and_the_counts() {
    let m = migrator();
    let all = [
      status_of(1, MigrationState::Applied),
      status_of(2, MigrationState::Pending),
      status_of(20_261_016_120_000, MigrationState::Drifted),
      status_of(7, MigrationState::Missing),
    ];
    let want = "\
id-effect migrate status: migrations (table _id_effect_migrations)
  applied               1  2026-10-16 21:09:02Z  m 1
  pending               2                        m 2  (reversible)
  drifted  20261016120000  2026-10-16 21:09:02Z  m 20261016120000
  missing               7  2026-10-16 21:09:02Z  m 7

1 applied, 1 pending, 1 drifted, 1 missing
";
    assert_eq!(status(Path::new("migrations"), &m, &all, false), want);
    let empty = status(Path::new("db"), &m, &[], false);
    assert!(
      empty.contains("  (no migrations)\n\n0 applied, 0 pending"),
      "{empty}"
    );
  }

  #[test]
  fn status_json_has_every_field() {
    let m = migrator().with_table("app.schema_migrations");
    let all = [
      status_of(1, MigrationState::Applied),
      status_of(2, MigrationState::Pending),
    ];
    let value: Value =
      serde_json::from_str(&status(Path::new("migrations"), &m, &all, true)).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["dir"], "migrations");
    assert_eq!(value["table"], "app.schema_migrations");
    assert_eq!(
      value["counts"],
      json!({"applied": 1, "pending": 1, "drifted": 0, "missing": 0})
    );
    assert_eq!(
      value["migrations"][0],
      json!({
        "version": 1,
        "description": "m 1",
        "state": "applied",
        "reversible": false,
        "checksum": "ab01",
        "applied_checksum": "cd",
        "applied_at": "2026-10-16T21:09:02.5Z",
      })
    );
    assert_eq!(value["migrations"][1]["applied_at"], Value::Null);
    assert_eq!(value["migrations"][1]["reversible"], true);
    let applied = [status_of(1, MigrationState::Applied)];
    let value: Value = serde_json::from_str(&status(Path::new("m"), &m, &applied, true)).unwrap();
    assert_eq!(value["ok"], true);
  }
}
