#![allow(clippy::new_ret_no_self, dead_code)]
//! Unified id_effect CLI: `version`, `new` generator scaffold, and diagnostics entrypoints.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use id_effect_cli::generator::{AppTemplate, ScaffoldError, ScaffoldOptions, scaffold_app};

#[derive(Parser, Debug)]
#[command(
  name = "id-effect",
  about = "id_effect developer CLI (Phase E parity)",
  version
)]
struct Cli {
  #[command(subcommand)]
  cmd: IdEffectCmd,
}

#[derive(Subcommand, Debug)]
enum IdEffectCmd {
  /// Print toolchain and crate version metadata.
  Version,
  /// Scaffold a new application crate from a built-in template.
  New {
    /// Crate / binary name (kebab-case).
    name: String,
    /// Output directory (defaults to `./<name>`).
    #[arg(long)]
    dest: Option<PathBuf>,
    /// Template to use.
    #[arg(long, default_value = "minimal")]
    template: NewTemplate,
    /// Relative path from dest to `id_effect` (workspace layouts).
    #[arg(long, default_value = "../../crates/id_effect")]
    id_effect_path: String,
    /// Relative path from dest to `id_effect_cli`.
    #[arg(long, default_value = "../../crates/id_effect_cli")]
    id_effect_cli_path: String,
  },
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum NewTemplate {
  Minimal,
}

impl From<NewTemplate> for AppTemplate {
  fn from(value: NewTemplate) -> Self {
    match value {
      NewTemplate::Minimal => AppTemplate::Minimal,
    }
  }
}

fn main() -> ExitCode {
  let cli = Cli::parse();
  match cli.cmd {
    IdEffectCmd::Version => {
      println!("id-effect {}", env!("CARGO_PKG_VERSION"));
      println!("id_effect workspace CLI — Phase E parity stub");
      ExitCode::SUCCESS
    }
    IdEffectCmd::New {
      name,
      dest,
      template,
      id_effect_path,
      id_effect_cli_path,
    } => match run_new(
      name,
      dest,
      template.into(),
      id_effect_path,
      id_effect_cli_path,
    ) {
      Ok(()) => ExitCode::SUCCESS,
      Err(code) => code,
    },
  }
}

fn run_new(
  name: String,
  dest: Option<PathBuf>,
  template: AppTemplate,
  id_effect_path: String,
  id_effect_cli_path: String,
) -> Result<(), ExitCode> {
  let dest = dest.unwrap_or_else(|| PathBuf::from(&name));
  let options = ScaffoldOptions {
    name: name.clone(),
    dest: dest.clone(),
    template,
    id_effect_path,
    id_effect_cli_path,
  };
  match scaffold_app(&options) {
    Ok(report) => {
      println!("created {} at {}", name, dest.display());
      for file in &report.files {
        println!("  {file}");
      }
      println!("\nNext: cd {} && cargo check", dest.display());
      Ok(())
    }
    Err(ScaffoldError::Exists(path)) => {
      eprintln!("error: destination already exists: {}", path.display());
      Err(ExitCode::from(1))
    }
    Err(ScaffoldError::Io(msg)) => {
      eprintln!("error: {msg}");
      Err(ExitCode::from(1))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;

  #[test]
  fn run_new_writes_temp_project() {
    let dest = std::env::temp_dir().join(format!("id_effect_new_bin_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dest);
    run_new(
      "tmp-app".into(),
      Some(dest.clone()),
      AppTemplate::Minimal,
      "../id_effect".into(),
      "../id_effect_cli".into(),
    )
    .expect("new");
    assert!(dest.join("Cargo.toml").exists());
    let _ = fs::remove_dir_all(dest);
  }
}
