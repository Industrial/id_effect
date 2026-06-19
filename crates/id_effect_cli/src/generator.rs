//! Stub project generator for `id-effect new`.

use std::fs;
use std::path::{Path, PathBuf};

/// Supported scaffold templates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppTemplate {
  /// Minimal clap + `run_main` binary (see `templates/app-minimal/`).
  Minimal,
}

impl AppTemplate {
  /// Template directory name under `templates/`.
  pub fn dir_name(self) -> &'static str {
    match self {
      Self::Minimal => "app-minimal",
    }
  }
}

/// Options for [`scaffold_app`].
#[derive(Clone, Debug)]
pub struct ScaffoldOptions {
  /// Crate / binary name (kebab-case recommended).
  pub name: String,
  /// Destination directory (created when missing).
  pub dest: PathBuf,
  /// Which template to materialize.
  pub template: AppTemplate,
  /// Relative path from dest to the workspace `id_effect` crate.
  pub id_effect_path: String,
  /// Relative path from dest to the workspace `id_effect_cli` crate.
  pub id_effect_cli_path: String,
}

/// Result of a successful scaffold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScaffoldReport {
  /// Root directory written.
  pub root: PathBuf,
  /// Files created (relative to `root`).
  pub files: Vec<String>,
}

/// Errors during template expansion.
#[derive(Debug, PartialEq, Eq)]
pub enum ScaffoldError {
  /// Destination already exists and is non-empty.
  Exists(PathBuf),
  /// I/O or template read failure.
  Io(String),
}

impl std::fmt::Display for ScaffoldError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Exists(p) => write!(f, "destination already exists: {}", p.display()),
      Self::Io(msg) => write!(f, "{msg}"),
    }
  }
}

impl std::error::Error for ScaffoldError {}

/// Materialize `options.template` into `options.dest`.
pub fn scaffold_app(options: &ScaffoldOptions) -> Result<ScaffoldReport, ScaffoldError> {
  if options.dest.exists() {
    let empty = fs::read_dir(&options.dest)
      .map_err(|e| ScaffoldError::Io(e.to_string()))?
      .next()
      .is_none();
    if !empty {
      return Err(ScaffoldError::Exists(options.dest.clone()));
    }
  } else {
    fs::create_dir_all(&options.dest).map_err(|e| ScaffoldError::Io(e.to_string()))?;
  }

  let template_root = template_root(options.template);
  let mut files = Vec::new();
  copy_template_tree(&template_root, &options.dest, options, &mut files)?;
  Ok(ScaffoldReport {
    root: options.dest.clone(),
    files,
  })
}

fn template_root(template: AppTemplate) -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("templates")
    .join(template.dir_name())
}

fn copy_template_tree(
  src: &Path,
  dest: &Path,
  options: &ScaffoldOptions,
  files: &mut Vec<String>,
) -> Result<(), ScaffoldError> {
  for entry in fs::read_dir(src).map_err(|e| ScaffoldError::Io(e.to_string()))? {
    let entry = entry.map_err(|e| ScaffoldError::Io(e.to_string()))?;
    let file_type = entry
      .file_type()
      .map_err(|e| ScaffoldError::Io(e.to_string()))?;
    let src_path = entry.path();
    let file_name = entry.file_name();
    let name = file_name.to_string_lossy();
    if file_type.is_dir() {
      let child_dest = dest.join(&*name);
      fs::create_dir_all(&child_dest).map_err(|e| ScaffoldError::Io(e.to_string()))?;
      copy_template_tree(&src_path, &child_dest, options, files)?;
      continue;
    }
    let rel = if dest == options.dest {
      name.to_string()
    } else {
      dest
        .strip_prefix(&options.dest)
        .ok()
        .and_then(|p| p.to_str())
        .map(|prefix| format!("{prefix}/{name}"))
        .unwrap_or_else(|| name.to_string())
    };
    let out_name = name.strip_suffix(".template").unwrap_or(&name);
    let out_path = if out_name == name {
      dest.join(&*name)
    } else {
      dest.join(out_name)
    };
    let raw = fs::read_to_string(&src_path).map_err(|e| ScaffoldError::Io(e.to_string()))?;
    let expanded = expand_template(&raw, options);
    if let Some(parent) = out_path.parent() {
      fs::create_dir_all(parent).map_err(|e| ScaffoldError::Io(e.to_string()))?;
    }
    fs::write(&out_path, expanded).map_err(|e| ScaffoldError::Io(e.to_string()))?;
    files.push(rel.replace('\\', "/"));
  }
  Ok(())
}

fn expand_template(input: &str, options: &ScaffoldOptions) -> String {
  input
    .replace("{{name}}", &options.name)
    .replace("{{id_effect_path}}", &options.id_effect_path)
    .replace("{{id_effect_cli_path}}", &options.id_effect_cli_path)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn scaffold_minimal_writes_files() {
    let dest = std::env::temp_dir().join(format!("id_effect_scaffold_test_{}", uuid_simple()));
    let _ = fs::remove_dir_all(&dest);
    let options = ScaffoldOptions {
      name: "demo-app".into(),
      dest: dest.clone(),
      template: AppTemplate::Minimal,
      id_effect_path: "../../crates/id_effect".into(),
      id_effect_cli_path: "../../crates/id_effect_cli".into(),
    };
    let report = scaffold_app(&options).expect("scaffold");
    assert!(report.files.iter().any(|f| f.contains("main.rs")));
    assert!(dest.join("src/main.rs").exists());
    let main_rs = fs::read_to_string(dest.join("src/main.rs")).unwrap();
    assert!(main_rs.contains("demo-app"));
    let _ = fs::remove_dir_all(dest);
  }

  #[test]
  fn expand_template_substitutes_placeholders() {
    let opts = ScaffoldOptions {
      name: "demo".into(),
      dest: PathBuf::from("/tmp"),
      template: AppTemplate::Minimal,
      id_effect_path: "ie".into(),
      id_effect_cli_path: "cli".into(),
    };
    let out = expand_template("{{name}} {{id_effect_path}}", &opts);
    assert_eq!(out, "demo ie");
  }

  #[test]
  fn scaffold_error_display_variants() {
    let exists = ScaffoldError::Exists(PathBuf::from("/x"));
    assert!(exists.to_string().contains("/x"));
    let io = ScaffoldError::Io("disk".into());
    assert!(io.to_string().contains("disk"));
  }

  #[test]
  fn app_template_dir_name() {
    assert_eq!(AppTemplate::Minimal.dir_name(), "app-minimal");
  }

  #[test]
  fn scaffold_refuses_nonempty_dest() {
    let dest = std::env::temp_dir().join(format!("id_effect_scaffold_exists_{}", uuid_simple()));
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("keep.txt"), "x").unwrap();
    let options = ScaffoldOptions {
      name: "demo-app".into(),
      dest: dest.clone(),
      template: AppTemplate::Minimal,
      id_effect_path: "../../crates/id_effect".into(),
      id_effect_cli_path: "../../crates/id_effect_cli".into(),
    };
    assert!(matches!(
      scaffold_app(&options),
      Err(ScaffoldError::Exists(_))
    ));
    let _ = fs::remove_dir_all(dest);
  }

  #[test]
  fn scaffold_writes_cargo_toml() {
    let dest = std::env::temp_dir().join(format!("id_effect_scaffold_cargo_{}", uuid_simple()));
    let _ = fs::remove_dir_all(&dest);
    let options = ScaffoldOptions {
      name: "cargo-app".into(),
      dest: dest.clone(),
      template: AppTemplate::Minimal,
      id_effect_path: "../../crates/id_effect".into(),
      id_effect_cli_path: "../../crates/id_effect_cli".into(),
    };
    scaffold_app(&options).expect("scaffold");
    assert!(dest.join("Cargo.toml").exists());
    let _ = fs::remove_dir_all(dest);
  }

  #[test]
  fn scaffold_report_lists_relative_paths() {
    let dest = std::env::temp_dir().join(format!("id_effect_scaffold_report_{}", uuid_simple()));
    let _ = fs::remove_dir_all(&dest);
    let options = ScaffoldOptions {
      name: "report-app".into(),
      dest: dest.clone(),
      template: AppTemplate::Minimal,
      id_effect_path: "../id_effect".into(),
      id_effect_cli_path: "../id_effect_cli".into(),
    };
    let report = scaffold_app(&options).expect("scaffold");
    assert_eq!(report.root, dest);
    assert!(!report.files.is_empty());
    let _ = fs::remove_dir_all(dest);
  }

  fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map(|d| d.as_nanos().to_string())
      .unwrap_or_else(|_| "0".into())
  }
}
