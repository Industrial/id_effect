//! The `id-effect` command line: argument parsing and [`run`], which `main` calls.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::doctor::{GraphFormat, Options as DoctorOptions, doctor};
use crate::migrate::MigrateArgs;
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
  /// Check that a project can build and run here: toolchain, id_effect versions, environment,
  /// database, docker, port (and, with --graph, its layer graph). Exits 1 when a check fails.
  Doctor(DoctorArgs),
  /// Create, apply, list, revert and check SQL migrations (PostgreSQL). Exits 1 when `check`
  /// finds pending or changed migrations, 3 on a database error.
  Migrate(MigrateArgs),
}

/// Arguments of `id-effect doctor`.
#[derive(Debug, Args)]
pub struct DoctorArgs {
  /// The project directory.
  #[arg(long, value_name = "DIR", default_value = ".")]
  pub path: PathBuf,
  /// Print the report as one JSON object.
  #[arg(long)]
  pub json: bool,
  /// Also build the project, run its binary with `--describe-layers` and print the layer graph
  /// (`--graph=dot` for Graphviz DOT).
  #[arg(
    long,
    value_enum,
    value_name = "FORMAT",
    num_args = 0..=1,
    require_equals = true,
    default_missing_value = "text"
  )]
  pub graph: Option<GraphFormat>,
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
/// Exit codes: `0` success, `1` the command failed, `2` a usage error, `3` a database error
/// (`migrate`; see [`crate::migrate`]).
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
    Command::Doctor(args) => run_doctor(&args, out),
    Command::Migrate(args) => crate::migrate::run(&args, out, err),
  }
}

fn run_doctor(args: &DoctorArgs, out: &mut dyn Write) -> ExitCode {
  let options = DoctorOptions { graph: args.graph };
  let report = doctor(&args.path, &options);
  let text = if args.json {
    report.to_json()
  } else {
    report.to_text()
  };
  let _ = write!(out, "{text}");
  if report.failed() {
    ExitCode::FAILURE
  } else {
    ExitCode::SUCCESS
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
      other => panic!("not `new`: {other:?}"),
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

    fn parse_doctor(args: &[&str]) -> DoctorArgs {
      let argv = ["id-effect", "doctor"].iter().chain(args).copied();
      match Cli::try_parse_from(argv).expect("parse").command {
        Command::Doctor(args) => args,
        other => panic!("not `doctor`: {other:?}"),
      }
    }

    #[test]
    fn doctor_defaults_to_this_directory_text_and_no_graph() {
      let args = parse_doctor(&[]);
      assert_eq!(args.path, PathBuf::from("."));
      assert!(!args.json);
      assert_eq!(args.graph, None);
    }

    #[test]
    fn doctor_reads_path_json_and_both_graph_forms() {
      let args = parse_doctor(&["--path", "/srv/app", "--json", "--graph"]);
      assert_eq!(args.path, PathBuf::from("/srv/app"));
      assert!(args.json);
      assert_eq!(args.graph, Some(GraphFormat::Text));
      assert_eq!(parse_doctor(&["--graph=dot"]).graph, Some(GraphFormat::Dot));
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
    fn doctor_on_a_directory_without_a_project_exits_1() {
      let tmp = tempfile::tempdir().unwrap();
      let dir = tmp.path().to_str().unwrap();
      let (code, out, _) = call(&["doctor", "--path", dir]);
      assert_eq!(code, ExitCode::FAILURE);
      assert!(out.contains("  fail  project  no Cargo.toml in "), "{out}");
      let (code, out, _) = call(&["doctor", "--path", dir, "--json"]);
      assert_eq!(code, ExitCode::FAILURE);
      assert!(
        out.starts_with("{\"path\":") && out.contains("\"ok\":false"),
        "{out}"
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
