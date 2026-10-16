//! The `id-effect` command line: argument parsing and [`run`], which `main` calls.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::project::new_project;
use crate::render::{Db, Options};
use crate::template::Template;

/// `id-effect`: the id_effect project tool.
#[derive(Debug, Parser)]
#[command(name = "id-effect", version, about = "The id_effect project tool")]
pub struct Cli {
  /// What to do.
  #[command(subcommand)]
  pub command: Command,
}

/// The `id-effect` subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
  /// Generate a new project from a template.
  New(NewArgs),
}

/// Arguments of `id-effect new`.
#[derive(Debug, Args)]
pub struct NewArgs {
  /// Package and binary name (lowercase ASCII letters, digits, `-`, `_`).
  pub name: String,
  /// The template to generate.
  #[arg(long, value_enum)]
  pub template: Template,
  /// Directory to create (default: `./<name>`); must be missing or empty.
  #[arg(long, value_name = "DIR")]
  pub dest: Option<PathBuf>,
  /// Database wiring (`service` only).
  #[arg(long, value_enum, default_value_t = Db::None)]
  pub db: Db,
  /// Export traces, metrics and logs over OTLP (`service` only).
  #[arg(long, overrides_with = "no_otel")]
  pub otel: bool,
  /// No OpenTelemetry export (the default).
  #[arg(long = "no-otel", overrides_with = "otel")]
  pub no_otel: bool,
  /// Build against the id_effect checkout at PATH instead of crates.io
  /// (writes a `[patch.crates-io]` section).
  #[arg(long, value_name = "PATH")]
  pub local_workspace: Option<PathBuf>,
}

impl NewArgs {
  /// The [`Options`] these arguments ask for.
  pub fn options(&self) -> Options {
    let options = Options::new(self.name.clone(), self.template)
      .db(self.db)
      .otel(self.otel && !self.no_otel);
    match &self.local_workspace {
      Some(path) => options.local_workspace(path.clone()),
      None => options,
    }
  }

  /// The destination directory: `--dest`, else `./<name>`.
  pub fn dest(&self) -> PathBuf {
    self
      .dest
      .clone()
      .unwrap_or_else(|| PathBuf::from(&self.name))
  }
}

/// Runs `id-effect` with `args` (including the program name), writing to `out` and `err`.
/// Exit codes: `0` success, `1` the command failed, `2` a usage error.
pub fn run<'a, I, T>(args: I, out: &'a mut dyn Write, err: &'a mut dyn Write) -> ExitCode
where
  I: IntoIterator<Item = T>,
  T: Into<OsString> + Clone,
{
  let cli = match Cli::try_parse_from(args) {
    Ok(cli) => cli,
    Err(e) => {
      let to = if e.use_stderr() { err } else { out };
      let _ = write!(to, "{}", e.render());
      return ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(2));
    }
  };
  match cli.command {
    Command::New(args) => run_new(&args, out, err),
  }
}

fn run_new(args: &NewArgs, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
  let dest = args.dest();
  match new_project(&args.options(), &dest) {
    Ok(report) => {
      let _ = writeln!(
        out,
        "Created {} project `{}` in {} ({} files).\n\nNext:\n  cd {}\n  cargo test\n  cargo run",
        args.template,
        args.name,
        report.root.display(),
        report.files.len(),
        report.root.display(),
      );
      ExitCode::SUCCESS
    }
    Err(e) => {
      let _ = writeln!(err, "error: {e}");
      ExitCode::FAILURE
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::error::NewError;

  fn parse(args: &[&str]) -> NewArgs {
    let mut argv = vec!["id-effect", "new"];
    argv.extend_from_slice(args);
    match Cli::try_parse_from(argv).expect("parse").command {
      Command::New(args) => args,
    }
  }

  fn call(args: &[&str]) -> (ExitCode, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(
      std::iter::once("id-effect").chain(args.iter().copied()),
      &mut out,
      &mut err,
    );
    let text = |b: Vec<u8>| String::from_utf8(b).expect("utf-8");
    (code, text(out), text(err))
  }

  mod parsing {
    use super::*;

    #[test]
    fn defaults_dest_db_and_otel() {
      let args = parse(&["shop", "--template", "service"]);
      assert_eq!(args.dest(), PathBuf::from("shop"));
      assert_eq!(args.options(), Options::new("shop", Template::Service));
    }

    #[test]
    fn reads_every_option() {
      let args = parse(&[
        "shop",
        "--template",
        "service",
        "--dest",
        "/tmp/x",
        "--db",
        "pg",
        "--otel",
        "--local-workspace",
        "/ws",
      ]);
      assert_eq!(args.dest(), PathBuf::from("/tmp/x"));
      let want = Options::new("shop", Template::Service)
        .db(Db::Pg)
        .otel(true)
        .local_workspace("/ws");
      assert_eq!(args.options(), want);
    }

    #[test]
    fn the_last_otel_flag_wins() {
      let on = parse(&["a", "--template", "service", "--no-otel", "--otel"]);
      assert!(on.options().otel);
      let off = parse(&["a", "--template", "service", "--otel", "--no-otel"]);
      assert!(!off.options().otel);
    }

    #[test]
    fn every_template_name_parses() {
      for template in Template::ALL {
        assert_eq!(
          parse(&["a", "--template", template.name()]).template,
          template
        );
      }
    }
  }

  mod run {
    use super::*;

    #[test]
    fn new_writes_the_project_and_prints_next_steps() {
      let tmp = tempfile::tempdir().unwrap();
      let dest = tmp.path().join("tool");
      let dest_arg = dest.to_str().unwrap();
      let (code, out, err) = call(&["new", "tool", "--template", "cli", "--dest", dest_arg]);
      assert_eq!(code, ExitCode::SUCCESS, "{err}");
      assert!(out.contains("Created cli project `tool`"), "{out}");
      assert!(out.contains("cargo test"), "{out}");
      assert!(dest.join("Cargo.toml").is_file());
    }

    #[test]
    fn a_refused_destination_exits_1_with_the_reason() {
      let tmp = tempfile::tempdir().unwrap();
      std::fs::write(tmp.path().join("x"), "").unwrap();
      let dest_arg = tmp.path().to_str().unwrap();
      let (code, out, err) = call(&["new", "app", "--template", "worker", "--dest", dest_arg]);
      assert_eq!(code, ExitCode::FAILURE);
      assert!(out.is_empty());
      let want = NewError::Exists(tmp.path().to_path_buf()).to_string();
      assert_eq!(err.trim_end(), format!("error: {want}"));
    }

    #[test]
    fn an_unsupported_option_exits_1() {
      let tmp = tempfile::tempdir().unwrap();
      let dest_arg = tmp.path().to_str().unwrap();
      let (code, _, err) = call(&[
        "new",
        "app",
        "--template",
        "cli",
        "--db",
        "pg",
        "--dest",
        dest_arg,
      ]);
      assert_eq!(code, ExitCode::FAILURE);
      assert!(
        err.contains("--db pg is not supported by the `cli` template"),
        "{err}"
      );
    }

    #[test]
    fn usage_errors_exit_2_on_stderr() {
      for args in [
        &["new", "app"][..],
        &["new", "app", "--template", "web"],
        &[],
      ] {
        let (code, out, err) = call(args);
        assert_eq!(code, ExitCode::from(2), "{args:?}");
        assert!(out.is_empty() && !err.is_empty(), "{args:?}: {err}");
      }
    }

    #[test]
    fn help_and_version_exit_0_on_stdout() {
      let (code, out, _) = call(&["new", "--help"]);
      assert_eq!(code, ExitCode::SUCCESS);
      assert!(out.contains("--local-workspace"), "{out}");
      let (code, out, _) = call(&["--version"]);
      assert_eq!(code, ExitCode::SUCCESS);
      assert!(out.contains(env!("CARGO_PKG_VERSION")), "{out}");
    }
  }
}
