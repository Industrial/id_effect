//! [`Project`]: the directory doctor looks at, its `Cargo.toml`, `.env.example` and `.env`, and
//! the small readers for them (the TOML subset Cargo writes, dotenv lines).

use std::fs;
use std::path::{Path, PathBuf};

use super::system::System;

/// A project directory with a `Cargo.toml`.
#[derive(Clone, Debug)]
pub(crate) struct Project {
  /// The directory (as given).
  pub dir: PathBuf,
  /// `Cargo.toml`.
  pub manifest: String,
  /// `.env.example`'s assignments, when it exists.
  pub example: Option<Vec<(String, String)>>,
  /// `.env`'s assignments (empty when it does not exist).
  pub dotenv: Vec<(String, String)>,
}

/// Where a variable's value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
  /// The process environment.
  Environment,
  /// The project's `.env`.
  DotEnv,
  /// The project's `.env.example` (a default the app would see only once copied).
  Example,
}

impl Source {
  /// `environment`, `.env`, `.env.example`.
  pub fn label(self) -> &'static str {
    match self {
      Self::Environment => "environment",
      Self::DotEnv => ".env",
      Self::Example => ".env.example",
    }
  }
}

impl Project {
  /// Reads the project at `dir`; `Err` says why it is not one.
  pub fn load(dir: &Path) -> Result<Self, String> {
    let manifest_path = dir.join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path).map_err(|e| match e.kind() {
      std::io::ErrorKind::NotFound => format!("no Cargo.toml in {}", dir.display()),
      _ => format!("{}: {e}", manifest_path.display()),
    })?;
    let read_env = |name: &str| {
      fs::read_to_string(dir.join(name))
        .ok()
        .map(|text| parse_dotenv(&text))
    };
    Ok(Self {
      dir: dir.to_path_buf(),
      manifest,
      example: read_env(".env.example"),
      dotenv: read_env(".env").unwrap_or_default(),
    })
  }

  /// The package name, or `None` for a virtual workspace manifest.
  pub fn package_name(&self) -> Option<String> {
    toml_entries(&self.manifest)
      .into_iter()
      .find(|e| e.table == "package" && e.key == "name")
      .and_then(|e| unquote(&e.value))
  }

  /// `key` from the process environment, else `.env`.
  pub fn var(&self, sys: &dyn System, key: &str) -> Option<(String, Source)> {
    sys
      .var(key)
      .map(|v| (v, Source::Environment))
      .or_else(|| lookup(&self.dotenv, key).map(|v| (v, Source::DotEnv)))
  }

  /// `key` from the process environment, `.env`, else `.env.example`.
  pub fn var_or_example(&self, sys: &dyn System, key: &str) -> Option<(String, Source)> {
    self.var(sys, key).or_else(|| {
      self
        .example
        .as_deref()
        .and_then(|example| lookup(example, key))
        .map(|v| (v, Source::Example))
    })
  }
}

fn lookup(pairs: &[(String, String)], key: &str) -> Option<String> {
  pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

/// The `KEY=value` lines of a dotenv file, in order: blank lines and `#` comments skipped, an
/// `export ` prefix and matching quotes dropped, ` #` comments cut from unquoted values.
pub(crate) fn parse_dotenv(text: &str) -> Vec<(String, String)> {
  text
    .lines()
    .filter_map(|line| {
      let line = line.trim();
      if line.is_empty() || line.starts_with('#') {
        return None;
      }
      let line = line.strip_prefix("export ").unwrap_or(line);
      let (key, value) = line.split_once('=')?;
      let key = key.trim();
      if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
      }
      let value = value.trim();
      let value = match unquote(value) {
        Some(inner) => inner,
        None => value
          .split_once(" #")
          .map_or(value, |(v, _)| v)
          .trim_end()
          .to_string(),
      };
      Some((key.to_string(), value))
    })
    .collect()
}

/// One `key = value` line of a TOML file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TomlEntry {
  /// The table header it is under (`package`, `workspace.package`; `[[package]]` → `package`).
  pub table: String,
  /// Which header occurrence it is under (counts every header), so `[[package]]` entries group.
  pub occurrence: usize,
  /// The key, dotted keys kept whole (`rust-version.workspace`).
  pub key: String,
  /// The raw value (quotes, arrays and comments as written).
  pub value: String,
}

/// The `key = value` lines of `text`, enough for `Cargo.toml` and `Cargo.lock` as Cargo writes
/// them (one assignment per line). Lines that are not assignments are skipped.
pub(crate) fn toml_entries(text: &str) -> Vec<TomlEntry> {
  let mut entries = Vec::new();
  let mut table = String::new();
  let mut occurrence = 0;
  for line in text.lines() {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    if line.starts_with('[') {
      let header = line.trim_start_matches('[');
      if let Some(end) = header.find(']') {
        table = header[..end].trim().to_string();
        occurrence += 1;
        continue;
      }
    }
    if let Some((key, value)) = line.split_once('=') {
      let key = key.trim().trim_matches('"');
      if !key.is_empty() && !key.contains(char::is_whitespace) {
        entries.push(TomlEntry {
          table: table.clone(),
          occurrence,
          key: key.to_string(),
          value: value.trim().to_string(),
        });
      }
    }
  }
  entries
}

/// The contents of a value that starts with a `"…"` or `'…'` string (anything after the closing
/// quote, such as a comment, is ignored); `None` for other values.
pub(crate) fn unquote(value: &str) -> Option<String> {
  let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
  let rest = &value[1..];
  let end = rest.find(quote)?;
  Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn dotenv_lines_parse_like_a_shell_would_read_them() {
    let text = "\
# comment
HOST=127.0.0.1
export PORT = 8080
COOKIE_KEY=
NAME=\"a b # not a comment\"
RUST_LOG=info # trailing comment
not an assignment
";
    let pairs = parse_dotenv(text);
    let want = [
      ("HOST", "127.0.0.1"),
      ("PORT", "8080"),
      ("COOKIE_KEY", ""),
      ("NAME", "a b # not a comment"),
      ("RUST_LOG", "info"),
    ];
    let got: Vec<(&str, &str)> = pairs
      .iter()
      .map(|(k, v)| (k.as_str(), v.as_str()))
      .collect();
    assert_eq!(got, want);
  }

  #[test]
  fn toml_entries_know_their_table_and_occurrence() {
    let text = "\
[package]
name = \"app\" # the name
rust-version.workspace = true

[[package]]
name = \"a\"
[[package]]
name = \"b\"
";
    let entries = toml_entries(text);
    assert_eq!(entries[0].table, "package");
    assert_eq!(unquote(&entries[0].value).as_deref(), Some("app"));
    assert_eq!(entries[1].key, "rust-version.workspace");
    assert_eq!(entries[1].value, "true");
    assert_ne!(entries[2].occurrence, entries[3].occurrence);
    assert_eq!(unquote("true"), None);
    assert_eq!(unquote("'x'"), Some("x".into()));
  }

  #[test]
  fn load_reads_the_manifest_and_env_files() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(
      Project::load(tmp.path())
        .unwrap_err()
        .starts_with("no Cargo.toml in ")
    );
    fs::write(
      tmp.path().join("Cargo.toml"),
      "[package]\nname = \"shop\"\n",
    )
    .unwrap();
    let bare = Project::load(tmp.path()).unwrap();
    assert_eq!(bare.package_name().as_deref(), Some("shop"));
    assert!(bare.example.is_none() && bare.dotenv.is_empty());
    fs::write(tmp.path().join(".env.example"), "PORT=8080\nHOST=0.0.0.0\n").unwrap();
    fs::write(tmp.path().join(".env"), "PORT=9000\n").unwrap();
    let project = Project::load(tmp.path()).unwrap();
    let sys = crate::doctor::fake::FakeSystem::default().var("HOST", "::1");
    assert_eq!(
      project.var(&sys, "PORT"),
      Some(("9000".into(), Source::DotEnv))
    );
    assert_eq!(
      project.var(&sys, "HOST"),
      Some(("::1".into(), Source::Environment))
    );
    assert_eq!(project.var(&sys, "RUST_LOG"), None);
    let sys = crate::doctor::fake::FakeSystem::default();
    assert_eq!(
      project.var_or_example(&sys, "HOST"),
      Some(("0.0.0.0".into(), Source::Example))
    );
  }
}
