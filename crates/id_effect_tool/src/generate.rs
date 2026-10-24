//! `id-effect generate`: add code to a project `id-effect new` generated.
//!
//! ```text
//! id-effect generate job <NAME> [--queue QUEUE] [--cron EXPR] [--path DIR]
//! id-effect generate admin <Entity> [--read-only] [--permissions PREFIX] [--path DIR]
//! ```
//!
//! ## `generate job`
//!
//! `generate job send_report` changes the project at DIR (default `.`):
//!
//! | File | Change |
//! |---|---|
//! | `src/jobs/send_report.rs` | new: `SendReport` (`#[derive(Schema)]`, the payload), its `Job` impl (`NAME` = `<crate>.send_report`, `QUEUE`, priority, max attempts, unique key, retry), `handle`, and a test draining a `MemoryJobQueue` with a `Worker` |
//! | `src/jobs/mod.rs` | `pub mod send_report;` and `registry.register(send_report::handle)?` in `registry()`; created with `JobEnv` and `registry()` when missing |
//! | `src/lib.rs` | `pub mod jobs;`, when missing |
//! | `Cargo.toml` | `id_effect_jobs` at the project's `id_effect` version, when missing; in a `--local-workspace` project also its `[patch.crates-io]` entries |
//!
//! `--cron EXPR` also gives the job `CRON` and `cron()` (an `enqueue_on_tick` entry), registers it
//! in `jobs::schedule()` and turns on `id_effect_jobs`' `cron` feature.
//!
//! Nothing is overwritten: an existing `src/jobs/<NAME>.rs`, or a module of that name in
//! `src/jobs/mod.rs`, is an error, and nothing is written when a check fails. A line it cannot
//! place safely (no `Ok(registry)` closing `registry()`, a dependency it cannot parse) is left out
//! and printed under `Do by hand`. Exit `0` done, `1` refused or failed, `2` usage.
//!
//! ## `generate admin`
//!
//! `generate admin Document` finds the `#[derive(Entity)]` struct `Document` in the library
//! (`src/lib.rs` and the modules under `src/`), reads its fields and changes the project:
//!
//! | File | Change |
//! |---|---|
//! | `src/admin/document.rs` | new: `admin()`, an `id_effect_admin::EntityAdmin<Document>` with list columns (the id and up to five more fields), a search box (text fields), filters (`bool` fields and text fields named like `status`) and sortable columns; secret-looking fields (`password_hash`, `api_token`, …) excluded from every page; `SLUG`, the offered `ACTIONS`, `register`, `permissions` and tests (deny by default, every page guarded) |
//! | `src/admin/mod.rs` | `pub mod document;`, `document::register(site)?` in `site()` and `document::permissions()?` in `permissions()`; created with `PREFIX`, `site()`, `permissions()`, `policy()` and `router()` when missing |
//! | `src/lib.rs` | `pub mod admin;`, when missing |
//! | `Cargo.toml` | `id_effect_admin` at the project's `id_effect` version and `id_effect_kit`'s `authz` feature, when missing; in a `--local-workspace` project also their `[patch.crates-io]` entries |
//!
//! Secure by default (ADR 0013 R1): `router()` mounts the site only with `.guard(g)`, every page
//! checks `admin.<entity>.<action>` and nobody holds one until a role is granted it. No flag makes
//! the admin public. `--read-only` offers the list and detail pages only: `policy()` refuses the
//! entity's create, update, delete and bulk permissions whatever a role is granted.
//! `--permissions admin.<name>` names the entity's permissions `admin.<name>.<action>` (and its
//! pages `/admin/<name>`); the default is the entity's name in snake case.
//!
//! Generating the same admin again changes nothing (each line is added once); a
//! `src/admin/<entity>.rs` that differs from what would be generated (edited by hand) is never
//! overwritten. A line it cannot place safely is printed under `Do by hand`.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Subcommand};

use crate::error::NewError;
use crate::render::{ID_EFFECT_VERSION, keep_lines_with, substitute};
use crate::template::FILES;

/// The longest job name: the generated code stays within rustfmt's line width.
pub const MAX_JOB_NAME: usize = 24;

/// The id_effect crates `id_effect_jobs` adds to a project's dependency graph (a
/// `--local-workspace` project patches each), and those its `cron` feature adds.
const JOBS_CRATES: [&str; 2] = ["id_effect_jobs", "id_effect_sql"];
const CRON_CRATES: [&str; 2] = ["id_effect_node", "id_effect_process"];

/// Names the generated job module imports or defines: a job type must not shadow one.
const RESERVED_TYPES: &[&str] = &[
  "Arc",
  "Box",
  "Cron",
  "CronError",
  "CronJob",
  "DrainReport",
  "Effect",
  "Executor",
  "Job",
  "JobContext",
  "JobEnv",
  "JobFailure",
  "JobOptions",
  "JobQueueEnv",
  "JobQueueExt",
  "MemoryJobQueue",
  "Mutex",
  "Option",
  "Priority",
  "Result",
  "Schedule",
  "Schema",
  "String",
  "TestExecutor",
  "UniqueKey",
  "UtcDateTime",
  "Vec",
  "WallClock",
  "Worker",
  "ZonedDateTime",
];

/// Rust keywords (2024 edition, incl. reserved), plus `tests` (the job module's own test module)
/// and `jobs`: a job module must not be one.
const RESERVED_NAMES: &[&str] = &[
  "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate", "do",
  "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in",
  "jobs", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
  "return", "self", "static", "struct", "super", "tests", "trait", "true", "try", "type", "typeof",
  "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// Arguments of `id-effect generate`.
#[derive(Debug, Args)]
pub struct GenerateArgs {
  /// What to generate.
  #[command(subcommand)]
  pub command: GenerateCommand,
}

/// The `id-effect generate` subcommands. Non-exhaustive: new generators are added in minor
/// releases.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum GenerateCommand {
  /// Add a background job (id_effect_jobs): `src/jobs/<NAME>.rs` with its payload, `Job` impl,
  /// handler and a test, registered in src/jobs/mod.rs; adds the id_effect_jobs dependency.
  Job(JobArgs),
  /// Add an admin back office for an entity (id_effect_admin): `src/admin/<entity>.rs` with its
  /// list columns, search and filters from the entity's fields, registered in src/admin/mod.rs,
  /// which mounts the site behind a guard; adds the id_effect_admin dependency.
  Admin(AdminArgs),
}

/// Arguments of `id-effect generate admin`.
#[derive(Debug, Args)]
pub struct AdminArgs {
  /// The entity's type name: a `#[derive(Entity)]` struct in the library (e.g. `Document`,
  /// module `src/admin/document.rs`).
  pub entity: String,
  /// Offer the list and detail pages only: the generated policy refuses the entity's create,
  /// update, delete and bulk permissions whatever a role is granted.
  #[arg(long)]
  pub read_only: bool,
  /// The entity's permission prefix, `admin.<name>`: its permissions are
  /// `admin.<name>.<action>` and its pages `/admin/<name>` (default: the entity's name in snake
  /// case). Nobody holds them until granted.
  #[arg(long, value_name = "PREFIX")]
  pub permissions: Option<String>,
  /// The project directory.
  #[arg(long, value_name = "DIR", default_value = ".")]
  pub path: PathBuf,
}

/// Arguments of `id-effect generate job`.
#[derive(Debug, Args)]
pub struct JobArgs {
  /// The job's module name: lowercase ASCII letters, digits and single `_` (e.g. `send_report`,
  /// type `SendReport`), at most 24 characters.
  pub name: String,
  /// The queue the job is enqueued on (its `Job::QUEUE`).
  #[arg(long, value_name = "QUEUE", default_value = "default")]
  pub queue: String,
  /// Also enqueue the job at every fire time of the cron expression EXPR (e.g. `0 6 * * *`):
  /// adds `cron()` to the job and to `jobs::schedule()`, and id_effect_jobs' `cron` feature.
  #[arg(long, value_name = "EXPR")]
  pub cron: Option<String>,
  /// The project directory.
  #[arg(long, value_name = "DIR", default_value = ".")]
  pub path: PathBuf,
}

/// Why `id-effect generate` refused or failed. Nothing was written, except for [`Io`](Self::Io)
/// while writing.
#[derive(Debug)]
#[non_exhaustive]
pub enum GenerateError {
  /// The job name is not a valid module name.
  InvalidName {
    /// The rejected name.
    name: String,
    /// What is wrong with it.
    reason: &'static str,
  },
  /// The queue name is not valid.
  InvalidQueue {
    /// The rejected queue.
    queue: String,
    /// What is wrong with it.
    reason: &'static str,
  },
  /// `--cron` is not a cron expression.
  InvalidCron {
    /// The rejected expression.
    expr: String,
    /// What is wrong with it.
    reason: String,
  },
  /// No `#[derive(Entity)]` struct of the name is in the library.
  UnknownEntity {
    /// The entity asked for.
    entity: String,
    /// What was found instead.
    reason: String,
  },
  /// The entity cannot get a generated admin (not a type name, private, generic, …).
  InvalidEntity {
    /// The entity asked for.
    entity: String,
    /// What is wrong with it.
    reason: String,
  },
  /// `--permissions` is not an admin permission prefix.
  InvalidPermissions {
    /// The rejected prefix.
    prefix: String,
    /// What is wrong with it.
    reason: &'static str,
  },
  /// The directory is not an id_effect project (`id-effect new` output).
  NotAProject {
    /// The directory.
    path: PathBuf,
    /// What is missing.
    reason: &'static str,
  },
  /// The job's file already exists.
  Exists(PathBuf),
  /// `src/jobs/mod.rs` already declares a module of the job's name.
  AlreadyDeclared {
    /// The module.
    module: String,
    /// The file declaring it.
    file: PathBuf,
  },
  /// A template is malformed.
  Template {
    /// The template file.
    file: String,
    /// What is wrong.
    reason: String,
  },
  /// Reading or writing `path` failed.
  Io {
    /// The path being read or written.
    path: PathBuf,
    /// The underlying error.
    source: io::Error,
  },
}

impl fmt::Display for GenerateError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::InvalidName { name, reason } => write!(f, "invalid job name `{name}`: {reason}"),
      Self::InvalidQueue { queue, reason } => write!(f, "invalid queue `{queue}`: {reason}"),
      Self::InvalidCron { expr, reason } => write!(f, "invalid --cron `{expr}`: {reason}"),
      Self::UnknownEntity { entity, reason } => write!(f, "unknown entity `{entity}`: {reason}"),
      Self::InvalidEntity { entity, reason } => write!(f, "invalid entity `{entity}`: {reason}"),
      Self::InvalidPermissions { prefix, reason } => {
        write!(f, "invalid --permissions `{prefix}`: {reason}")
      }
      Self::NotAProject { path, reason } => write!(
        f,
        "{} is not an id_effect project ({reason}); run it in a project `id-effect new` generated, or pass --path",
        path.display()
      ),
      Self::Exists(path) => write!(f, "{} already exists; nothing was written", path.display()),
      Self::AlreadyDeclared { module, file } => write!(
        f,
        "{} already declares `mod {module}`; nothing was written",
        file.display()
      ),
      Self::Template { file, reason } => write!(f, "template {file}: {reason}"),
      Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
    }
  }
}

impl std::error::Error for GenerateError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::Io { source, .. } => Some(source),
      _ => None,
    }
  }
}

impl From<NewError> for GenerateError {
  fn from(error: NewError) -> Self {
    match error {
      NewError::Template { file, reason } => Self::Template { file, reason },
      NewError::Io { path, source } => Self::Io { path, source },
      other => Self::Template {
        file: "generate/job".to_owned(),
        reason: other.to_string(),
      },
    }
  }
}

/// One file `generate` created or changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
  /// Path relative to the project, `/`-separated.
  pub path: String,
  /// `true` for a new file.
  pub created: bool,
  /// What changed in it.
  pub what: String,
}

/// What `generate job` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobReport {
  /// The job's type, e.g. `SendReport`.
  pub job_type: String,
  /// Its `Job::NAME`, e.g. `orders_api.send_report`.
  pub job_name: String,
  /// The files written, in order.
  pub changes: Vec<Change>,
  /// Lines it could not place safely: add them by hand.
  pub by_hand: Vec<String>,
}

/// Runs `id-effect generate`: the report on `out`, a failure (exit `1`) on `err`.
pub fn run(args: &GenerateArgs, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
  match &args.command {
    GenerateCommand::Job(job) => {
      match generate_job(&job.path, &job.name, &job.queue, job.cron.as_deref()) {
        Ok(report) => {
          let _ = write!(out, "{}", report_text(&report, job));
          ExitCode::SUCCESS
        }
        Err(e) => {
          let _ = writeln!(err, "error: {e}");
          ExitCode::FAILURE
        }
      }
    }
    GenerateCommand::Admin(admin) => {
      let prefix = admin.permissions.as_deref();
      match generate_admin(&admin.path, &admin.entity, admin.read_only, prefix) {
        Ok(report) => {
          let _ = write!(out, "{}", admin_report_text(&report));
          ExitCode::SUCCESS
        }
        Err(e) => {
          let _ = writeln!(err, "error: {e}");
          ExitCode::FAILURE
        }
      }
    }
  }
}

fn report_text(report: &JobReport, job: &JobArgs) -> String {
  let mut text = format!(
    "Generated job `{}` ({}, queue `{}`):\n",
    job.name, report.job_name, job.queue
  );
  for change in &report.changes {
    let action = if change.created { "created" } else { "updated" };
    text.push_str(&format!("  {action}  {}: {}\n", change.path, change.what));
  }
  if !report.by_hand.is_empty() {
    text.push_str("\nDo by hand:\n");
    for line in &report.by_hand {
      text.push_str(&format!("  {line}\n"));
    }
  }
  text.push_str(&format!(
    "\nNext:\n  cargo test jobs::{name}\n  Enqueue one: id_effect_jobs::enqueue({ty} {{ id: 1 }}, JobOptions::default())\n  Run them: Worker::new(jobs::registry()?).queue(\"{queue}\", 4).run(), provided with\n    job_queue_env(PgJobQueue::new(sql_client)) (or pg_job_queue_layer())\n",
    name = job.name,
    ty = report.job_type,
    queue = job.queue,
  ));
  if job.cron.is_some() {
    text.push_str("  Schedule it: run jobs::schedule()? under an id_effect_jobs::CronLeader\n");
  }
  text
}

/// Adds the job `name` to the project at `project` (see the [module docs](self)): enqueued on
/// `queue`, and with `cron` also at every fire time of that expression.
///
/// # Errors
///
/// [`GenerateError::InvalidName`], [`InvalidQueue`](GenerateError::InvalidQueue),
/// [`InvalidCron`](GenerateError::InvalidCron), [`NotAProject`](GenerateError::NotAProject),
/// [`Exists`](GenerateError::Exists) or [`AlreadyDeclared`](GenerateError::AlreadyDeclared) before
/// anything is written; [`Io`](GenerateError::Io) when reading or writing fails.
pub fn generate_job(
  project: &Path,
  name: &str,
  queue: &str,
  cron: Option<&str>,
) -> Result<JobReport, GenerateError> {
  let job_type = validate_job_name(name)?;
  validate_queue(queue)?;
  if let Some(expr) = cron {
    validate_cron(expr)?;
  }
  let not_a_project = |reason| GenerateError::NotAProject {
    path: project.to_path_buf(),
    reason,
  };
  let cargo_path = project.join("Cargo.toml");
  let lib_path = project.join("src/lib.rs");
  let mod_path = project.join("src/jobs/mod.rs");
  let job_path = project.join(format!("src/jobs/{name}.rs"));
  if !cargo_path.is_file() {
    return Err(not_a_project("no Cargo.toml"));
  }
  let cargo = read(&cargo_path)?;
  let crate_name = package_name(&cargo)
    .ok_or_else(|| not_a_project("no [package] name in Cargo.toml"))?
    .replace('-', "_");
  if dependency(&cargo, "id_effect").is_none() && dependency(&cargo, "id_effect_kit").is_none() {
    return Err(not_a_project("Cargo.toml does not depend on id_effect"));
  }
  if !lib_path.is_file() {
    return Err(not_a_project("no src/lib.rs"));
  }
  if job_path.exists() {
    return Err(GenerateError::Exists(job_path));
  }
  let lib = read(&lib_path)?;
  let existing_mod = if mod_path.is_file() {
    Some(read(&mod_path)?)
  } else {
    None
  };
  if let Some(text) = &existing_mod
    && declares_mod(text, name)
  {
    return Err(GenerateError::AlreadyDeclared {
      module: name.to_owned(),
      file: mod_path,
    });
  }

  let vars = [
    ("job", name),
    ("job_type", job_type.as_str()),
    ("crate_name", crate_name.as_str()),
    ("queue", queue),
    ("cron", cron.unwrap_or_default()),
  ];
  let job_text = render("job.rs.template", cron.is_some(), &vars)?;
  let mut by_hand = Vec::new();
  let mut changes = vec![Change {
    path: format!("src/jobs/{name}.rs"),
    created: true,
    what: format!("{job_type} (payload), its Job impl, handle and a test"),
  }];

  // src/jobs/mod.rs: the module, its registration and (--cron) its schedule entry.
  let mut jobs_mod = match &existing_mod {
    Some(text) => text.clone(),
    None => render("mod.rs.template", false, &vars)?,
  };
  let mut mod_what = vec![format!("pub mod {name}")];
  jobs_mod = insert_mod(&jobs_mod, name);
  let register = format!("let registry = registry.register({name}::handle)?;");
  match insert_before_ok(&jobs_mod, "registry", &register) {
    Some(text) => {
      jobs_mod = text;
      mod_what.push(format!("{name}::handle registered in registry()"));
    }
    None => by_hand.push(format!(
      "src/jobs/mod.rs: add `{register}` to the registry() the worker runs"
    )),
  }
  if cron.is_some() {
    if !jobs_mod.contains("pub fn schedule(") {
      jobs_mod.push_str(&render("schedule.rs.template", false, &vars)?);
      mod_what.push("schedule()".to_owned());
    }
    let entry = format!("let schedule = schedule.with({name}::cron()?)?;");
    match insert_before_ok(&jobs_mod, "schedule", &entry) {
      Some(text) => {
        jobs_mod = text;
        mod_what.push(format!("{name}::cron() added to schedule()"));
      }
      None => by_hand.push(format!(
        "src/jobs/mod.rs: add `{entry}` to the schedule() the cron leader runs"
      )),
    }
  }
  changes.push(Change {
    path: "src/jobs/mod.rs".to_owned(),
    created: existing_mod.is_none(),
    what: mod_what.join(", "),
  });

  // src/lib.rs: the `jobs` module.
  let new_lib = if declares_mod(&lib, "jobs") {
    None
  } else {
    changes.push(Change {
      path: "src/lib.rs".to_owned(),
      created: false,
      what: "pub mod jobs".to_owned(),
    });
    Some(insert_mod(&lib, "jobs"))
  };

  // Cargo.toml: the dependency (and its patches).
  let (new_cargo, cargo_what) = add_jobs_dependency(&cargo, cron.is_some(), &mut by_hand);
  if !cargo_what.is_empty() {
    changes.push(Change {
      path: "Cargo.toml".to_owned(),
      created: false,
      what: cargo_what.join(", "),
    });
  }

  let jobs_dir = project.join("src/jobs");
  fs::create_dir_all(&jobs_dir).map_err(|source| GenerateError::Io {
    path: jobs_dir,
    source,
  })?;
  write_new(&job_path, &job_text)?;
  if existing_mod.is_some() {
    write(&mod_path, &jobs_mod)?;
  } else {
    write_new(&mod_path, &jobs_mod)?;
  }
  if let Some(text) = new_lib {
    write(&lib_path, &text)?;
  }
  if new_cargo != cargo {
    write(&cargo_path, &new_cargo)?;
  }
  Ok(JobReport {
    job_type,
    job_name: format!("{crate_name}.{name}"),
    changes,
    by_hand,
  })
}

/// Checks `name` and returns its type name (`send_report` → `SendReport`).
fn validate_job_name(name: &str) -> Result<String, GenerateError> {
  let invalid = |reason| {
    Err(GenerateError::InvalidName {
      name: name.to_owned(),
      reason,
    })
  };
  let Some(first) = name.chars().next() else {
    return invalid("it is empty");
  };
  if name.len() > MAX_JOB_NAME {
    return invalid("it is longer than 24 characters");
  }
  if !first.is_ascii_lowercase() {
    return invalid("it must start with a lowercase ASCII letter");
  }
  if !name
    .chars()
    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
  {
    return invalid("use only lowercase ASCII letters, digits and `_` (snake_case)");
  }
  if name.ends_with('_') || name.contains("__") {
    return invalid("an `_` must sit between two words");
  }
  if RESERVED_NAMES.contains(&name) {
    return invalid("it is a Rust keyword or a name the jobs module uses");
  }
  let job_type: String = name
    .split('_')
    .map(|word| {
      let mut chars = word.chars();
      chars
        .next()
        .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
    })
    .collect();
  if RESERVED_TYPES.contains(&job_type.as_str()) {
    return invalid("its type name is one the job module imports");
  }
  Ok(job_type)
}

fn validate_queue(queue: &str) -> Result<(), GenerateError> {
  let invalid = |reason| {
    Err(GenerateError::InvalidQueue {
      queue: queue.to_owned(),
      reason,
    })
  };
  if queue.is_empty() {
    return invalid("it is empty");
  }
  if queue.len() > 64 {
    return invalid("it is longer than 64 characters");
  }
  if !queue
    .chars()
    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
  {
    return invalid("use only ASCII letters, digits, `_`, `-`, `.` and `:`");
  }
  Ok(())
}

fn validate_cron(expr: &str) -> Result<(), GenerateError> {
  let invalid = |reason: String| {
    Err(GenerateError::InvalidCron {
      expr: expr.to_owned(),
      reason,
    })
  };
  // The expression goes into a Rust string literal as is.
  if expr
    .chars()
    .any(|c| c == '"' || c == '\\' || c.is_control())
  {
    return invalid("it holds a quote, a backslash or a control character".to_owned());
  }
  match id_effect::Cron::parse(expr) {
    Ok(_) => Ok(()),
    Err(e) => invalid(e.to_string()),
  }
}

/// Renders the `generate/job` template `source` with the flag `cron` and `vars`.
fn render(source: &str, cron: bool, vars: &[(&str, &str)]) -> Result<String, GenerateError> {
  render_in("generate/job", source, &[("cron", cron)], vars)
}

/// Renders the template `dir/source` with the `{{#if}}` flags `flags` and `vars`.
fn render_in(
  dir: &str,
  source: &str,
  flags: &[(&str, bool)],
  vars: &[(&str, &str)],
) -> Result<String, GenerateError> {
  let label = format!("{dir}/{source}");
  let file = FILES
    .iter()
    .find(|f| f.dir == dir && f.source == source)
    .ok_or_else(|| GenerateError::Template {
      file: label.clone(),
      reason: "not embedded".to_owned(),
    })?;
  let lookup = |flag: &str| flags.iter().find(|(f, _)| *f == flag).map(|(_, on)| *on);
  let kept = keep_lines_with(file.contents, &lookup, &label)?;
  substitute(&kept, vars, &label).map_err(GenerateError::from)
}

fn read(path: &Path) -> Result<String, GenerateError> {
  fs::read_to_string(path).map_err(|source| GenerateError::Io {
    path: path.to_path_buf(),
    source,
  })
}

fn write(path: &Path, text: &str) -> Result<(), GenerateError> {
  fs::write(path, text).map_err(|source| GenerateError::Io {
    path: path.to_path_buf(),
    source,
  })
}

/// Writes a new file, refusing one that appeared since the checks.
fn write_new(path: &Path, text: &str) -> Result<(), GenerateError> {
  let io = |source| GenerateError::Io {
    path: path.to_path_buf(),
    source,
  };
  let mut file = fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(path)
    .map_err(io)?;
  file.write_all(text.as_bytes()).map_err(io)
}

/// The module a top-level `mod x;` / `pub mod x;` line declares.
fn mod_decl(line: &str) -> Option<&str> {
  let rest = line
    .strip_prefix("pub mod ")
    .or_else(|| line.strip_prefix("mod "))?;
  let name = rest.trim_end().strip_suffix(';')?.trim();
  name
    .chars()
    .all(|c| c.is_ascii_alphanumeric() || c == '_')
    .then_some(name)
}

/// Whether `text` declares the module `module` at the top level (`mod`, `pub mod`,
/// `pub(crate) mod`, with a body or not).
fn declares_mod(text: &str, module: &str) -> bool {
  text.lines().any(|line| {
    let rest = line
      .strip_prefix("pub(crate) ")
      .or_else(|| line.strip_prefix("pub "))
      .unwrap_or(line);
    rest
      .strip_prefix("mod ")
      .and_then(|r| r.strip_prefix(module))
      .is_some_and(|r| matches!(r.trim_start().chars().next(), Some(';' | '{')))
  })
}

/// Adds `pub mod module;` to `text`: in name order into its first run of top-level module
/// declarations (rustfmt sorts such a run), else after the leading `//!` / `#![…]` lines.
fn insert_mod(text: &str, module: &str) -> String {
  let decl = format!("pub mod {module};\n");
  let lines: Vec<&str> = text.split_inclusive('\n').collect();
  if let Some(start) = lines.iter().position(|l| mod_decl(l).is_some()) {
    let end = start
      + lines[start..]
        .iter()
        .take_while(|l| mod_decl(l).is_some())
        .count();
    let at = (start..end)
      .find(|&i| mod_decl(lines[i]).is_some_and(|m| m > module))
      .unwrap_or(end);
    let mut out = lines[..at].concat();
    if at == lines.len() && !out.is_empty() && !out.ends_with('\n') {
      out.push('\n');
    }
    out.push_str(&decl);
    out.push_str(&lines[at..].concat());
    return out;
  }
  let header = lines
    .iter()
    .take_while(|l| {
      let t = l.trim_start();
      t.starts_with("//!") || t.starts_with("#![")
    })
    .count();
  let body = header
    + lines[header..]
      .iter()
      .take_while(|l| l.trim().is_empty())
      .count();
  let mut out = lines[..header].concat();
  if header > 0 {
    out.push('\n');
  }
  out.push_str(&decl);
  if body < lines.len() {
    out.push('\n');
    out.push_str(&lines[body..].concat());
  }
  out
}

/// Inserts `line` before the `Ok(var)` that ends `pub fn var(`, indented like it; `None` when
/// the function or its `Ok(var)` is not found.
fn insert_before_ok(text: &str, var: &str, line: &str) -> Option<String> {
  let lines: Vec<&str> = text.split_inclusive('\n').collect();
  let open = format!("pub fn {var}(");
  let close = format!("Ok({var})");
  let start = lines.iter().position(|l| l.starts_with(&open))?;
  let at = (start + 1..lines.len())
    .take_while(|&i| !lines[i].starts_with('}'))
    .find(|&i| lines[i].trim() == close)?;
  let indent = &lines[at][..lines[at].len() - lines[at].trim_start().len()];
  let mut out = lines[..at].concat();
  out.push_str(&format!("{indent}{line}\n"));
  out.push_str(&lines[at..].concat());
  Some(out)
}

/// The `[name]` of a TOML table header line.
fn header(line: &str) -> Option<&str> {
  let t = line.trim();
  t.strip_prefix('[')?.strip_suffix(']').map(str::trim)
}

/// The key of a `key = value` line (`None` for comments, headers and continuations).
fn key(line: &str) -> Option<&str> {
  let t = line.trim();
  if t.starts_with('#') || t.starts_with('[') {
    return None;
  }
  let (key, _) = t.split_once('=')?;
  Some(key.trim().trim_matches('"'))
}

/// The line range of the table `name`'s entries (after its header, up to the next header).
fn table(lines: &[&str], name: &str) -> Option<(usize, usize)> {
  let head = lines.iter().position(|l| header(l) == Some(name))?;
  let end = (head + 1..lines.len())
    .find(|&i| header(lines[i]).is_some())
    .unwrap_or(lines.len());
  Some((head + 1, end))
}

/// The index of `key`'s line in `table`, if any.
fn entry(lines: &[&str], table_name: &str, key_name: &str) -> Option<usize> {
  let (start, end) = table(lines, table_name)?;
  (start..end).find(|&i| key(lines[i]) == Some(key_name))
}

/// The line declaring `name` in `[dependencies]`.
fn dependency<'a>(cargo: &'a str, name: &str) -> Option<&'a str> {
  let lines: Vec<&str> = cargo.lines().collect();
  entry(&lines, "dependencies", name).map(|i| lines[i])
}

/// `[package] name`.
fn package_name(cargo: &str) -> Option<String> {
  let lines: Vec<&str> = cargo.lines().collect();
  let line = lines[entry(&lines, "package", "name")?];
  let (_, value) = line.split_once('=')?;
  let name = value.trim().trim_matches('"');
  (!name.is_empty()).then(|| name.to_owned())
}

/// The version requirement of a dependency line: `"1.2"` or `{ version = "1.2", … }`.
fn version_of(line: &str) -> Option<&str> {
  let (_, value) = line.split_once('=')?;
  let value = value.trim();
  let quoted = match value.strip_prefix('"') {
    Some(rest) => rest,
    None => value
      .split_once("version")?
      .1
      .trim_start()
      .strip_prefix('=')?
      .trim_start()
      .strip_prefix('"')?,
  };
  quoted.split_once('"').map(|(v, _)| v)
}

fn view(lines: &[String]) -> Vec<&str> {
  lines.iter().map(String::as_str).collect()
}

/// The last non-blank line of `start..end`, or the header before it.
fn last_entry(lines: &[&str], start: usize, end: usize) -> usize {
  (start..end)
    .rev()
    .find(|&i| !lines[i].trim().is_empty())
    .unwrap_or(start - 1)
}

/// Adds `id_effect_jobs` (with `cron`: its `cron` feature) to `cargo`'s `[dependencies]`, and
/// the crates it brings to a `[patch.crates-io]` that points id_effect at a checkout. Returns the
/// new text and what changed; a change it cannot make is pushed on `by_hand`.
fn add_jobs_dependency(
  cargo: &str,
  cron: bool,
  by_hand: &mut Vec<String>,
) -> (String, Vec<String>) {
  let mut lines: Vec<String> = cargo.lines().map(str::to_owned).collect();
  let mut what = Vec::new();

  let version = dependency(cargo, "id_effect")
    .and_then(version_of)
    .unwrap_or(ID_EFFECT_VERSION)
    .to_owned();
  let features = if cron { ", features = [\"cron\"]" } else { "" };
  let dep = if cron {
    format!("id_effect_jobs = {{ version = \"{version}\"{features} }}")
  } else {
    format!("id_effect_jobs = \"{version}\"")
  };
  let current = view(&lines);
  match entry(&current, "dependencies", "id_effect_jobs") {
    None => {
      if let Some((start, end)) = table(&current, "dependencies") {
        let at = entry(&current, "dependencies", "id_effect")
          .unwrap_or_else(|| last_entry(&current, start, end));
        let comment =
          "# Background jobs (src/jobs/, `id-effect generate job`): queue, workers, retries.";
        lines.splice(at + 1..at + 1, [comment.to_owned(), dep.clone()]);
        what.push(dep);
      } else {
        by_hand.push(format!("Cargo.toml: add `{dep}` under [dependencies]"));
      }
    }
    Some(i) if cron && !lines[i].contains("\"cron\"") => {
      let line = lines[i].trim_end().to_owned();
      let value = line
        .split_once('=')
        .map(|(_, v)| v.trim())
        .unwrap_or_default();
      let new = if value.starts_with('"') && value.ends_with('"') {
        Some(format!(
          "id_effect_jobs = {{ version = {value}{features} }}"
        ))
      } else if line.contains("features = []") {
        Some(line.replacen("features = []", "features = [\"cron\"]", 1))
      } else if line.contains("features = [") {
        Some(line.replacen("features = [", "features = [\"cron\", ", 1))
      } else {
        line
          .strip_suffix('}')
          .map(|head| format!("{}{features} }}", head.trim_end()))
      };
      match new {
        Some(new) if new.ends_with('}') => {
          what.push("id_effect_jobs feature `cron`".to_owned());
          lines[i] = new;
        }
        _ => by_hand.push("Cargo.toml: enable id_effect_jobs' `cron` feature".to_owned()),
      }
    }
    Some(_) => {}
  }

  // A `--local-workspace` project: patch the new crates at the same checkout.
  let crates: Vec<&str> = JOBS_CRATES
    .iter()
    .chain(if cron { &CRON_CRATES[..] } else { &[] })
    .copied()
    .collect();
  add_patches(&mut lines, &crates, &mut what, by_hand);

  let mut text = lines.join("\n");
  if cargo.ends_with('\n') {
    text.push('\n');
  }
  (text, what)
}

/// Adds to a `[patch.crates-io]` that points id_effect at a checkout (a `--local-workspace`
/// project) the `crates` it does not patch yet, at the same checkout; a project without that
/// table is left alone. What it adds is pushed on `what`, what it cannot on `by_hand`.
fn add_patches(
  lines: &mut Vec<String>,
  crates: &[&str],
  what: &mut Vec<String>,
  by_hand: &mut Vec<String>,
) {
  let current = view(lines);
  let Some((start, end)) = table(&current, "patch.crates-io") else {
    return;
  };
  let root = entry(&current, "patch.crates-io", "id_effect").and_then(|i| {
    let path = current[i]
      .split_once("path")?
      .1
      .trim_start()
      .strip_prefix('=')?;
    let path = path.trim_start().strip_prefix('"')?.split_once('"')?.0;
    path.strip_suffix("/crates/id_effect").map(str::to_owned)
  });
  let wanted = crates
    .iter()
    .filter(|name| entry(&current, "patch.crates-io", name).is_none())
    .copied()
    .collect::<Vec<_>>();
  if wanted.is_empty() {
    return;
  }
  let at = last_entry(&current, start, end) + 1;
  match root {
    Some(root) => {
      let patches = wanted
        .iter()
        .map(|name| format!("{name} = {{ path = \"{root}/crates/{name}\" }}"))
        .collect::<Vec<_>>();
      lines.splice(at..at, patches);
      what.push(format!("[patch.crates-io] {}", wanted.join(", ")));
    }
    None => by_hand.push(format!(
      "Cargo.toml: patch {} in [patch.crates-io] like id_effect",
      wanted.join(", ")
    )),
  }
}

// ---------------------------------------------------------------------------------------------
// `id-effect generate admin`
// ---------------------------------------------------------------------------------------------

/// The path the generated admin code imports `id_effect_admin`'s items from (the `{{admin}}`
/// variable of `templates/generate/admin`): the crate itself, until `id_effect_kit` has an
/// `admin` feature and the import becomes `id_effect_kit::admin`.
pub const ADMIN_IMPORT: &str = "id_effect_admin";

/// The longest entity type name `generate admin` takes.
pub const MAX_ENTITY_NAME: usize = 64;

/// The id_effect crates `id_effect_admin` and the kit's `authz` feature add to a project's
/// dependency graph (a `--local-workspace` project patches each).
const ADMIN_CRATES: [&str; 4] = [
  "id_effect_admin",
  "id_effect_authz",
  "id_effect_axum",
  "id_effect_sql",
];

/// List columns besides the id, search fields and filters an admin gets at most.
const MAX_LIST: usize = 5;
const MAX_SEARCH: usize = 3;
const MAX_FILTERS: usize = 4;

/// Words (between `_`) of a field name that hold a secret: such a field is excluded from every
/// admin page (secure by default; show it again by editing the generated module).
const SECRET_WORDS: &[&str] = &[
  "credential",
  "credentials",
  "hash",
  "otp",
  "passwd",
  "password",
  "salt",
  "secret",
  "secrets",
  "token",
  "tokens",
  "totp",
];

/// Text fields with these names (or ending in `_status`) get a filter rather than the search box.
const FILTER_NAMES: &[&str] = &[
  "category", "kind", "level", "priority", "role", "stage", "state", "status", "type",
];

/// What `generate admin` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminReport {
  /// The entity's type, e.g. `Document`.
  pub entity: String,
  /// Its admin module, `src/admin/<module>.rs`, e.g. `document`.
  pub module: String,
  /// Its name in URLs and permissions, e.g. `document`.
  pub slug: String,
  /// The permissions its admin offers, e.g. `admin.document.list`.
  pub permissions: Vec<String>,
  /// The files written, in order: empty when the admin was generated before.
  pub changes: Vec<Change>,
  /// Lines it could not place safely: add them by hand.
  pub by_hand: Vec<String>,
}

fn admin_report_text(report: &AdminReport) -> String {
  let mut text = format!(
    "Generated the admin of `{}` (pages /admin/{slug}, permissions admin.{slug}.<action>):\n",
    report.entity,
    slug = report.slug,
  );
  if report.changes.is_empty() {
    text.push_str("  up to date: nothing was written\n");
  }
  for change in &report.changes {
    let action = if change.created { "created" } else { "updated" };
    text.push_str(&format!("  {action}  {}: {}\n", change.path, change.what));
  }
  if !report.by_hand.is_empty() {
    text.push_str("\nDo by hand:\n");
    for line in &report.by_hand {
      text.push_str(&format!("  {line}\n"));
    }
  }
  text.push_str(&format!(
    "\nOffered (nobody holds them until a role is granted them):\n  {}\n",
    report.permissions.join(", ")
  ));
  text.push_str(&format!(
    "\nNext:\n  cargo test admin::{module}\n  Grant them: Rbac::builder().grant(\"staff\", admin::{module}::permissions()?.iter().map(ToString::to_string)).build()\n  Mount it: admin::router(Guard::new(\"staff\", require_role(\"staff\")), rbac, AdminDatabase::new(sql_client, Dialect::Postgres))?,\n    merged into the app's router inside its authentication layer and SecurityLayer\n",
    module = report.module,
  ));
  text
}

/// Adds the admin of the entity `entity` to the project at `project` (see the
/// [module docs](self)): `--read-only` with `read_only`, its permissions named
/// `admin.<name>.<action>` with `permissions` = `admin.<name>`.
///
/// # Errors
///
/// [`GenerateError::InvalidEntity`], [`InvalidPermissions`](GenerateError::InvalidPermissions),
/// [`NotAProject`](GenerateError::NotAProject), [`UnknownEntity`](GenerateError::UnknownEntity),
/// [`Exists`](GenerateError::Exists) (an edited `src/admin/<entity>.rs`) or
/// [`AlreadyDeclared`](GenerateError::AlreadyDeclared) before anything is written;
/// [`Io`](GenerateError::Io) when reading or writing fails.
pub fn generate_admin(
  project: &Path,
  entity: &str,
  read_only: bool,
  permissions: Option<&str>,
) -> Result<AdminReport, GenerateError> {
  let module = validate_entity_name(entity)?;
  let slug = match permissions {
    Some(prefix) => permission_slug(prefix)?,
    None => module.clone(),
  };
  let not_a_project = |reason| GenerateError::NotAProject {
    path: project.to_path_buf(),
    reason,
  };
  let cargo_path = project.join("Cargo.toml");
  let lib_path = project.join("src/lib.rs");
  if !cargo_path.is_file() {
    return Err(not_a_project("no Cargo.toml"));
  }
  let cargo = read(&cargo_path)?;
  if dependency(&cargo, "id_effect_kit").is_none() {
    return Err(not_a_project("Cargo.toml does not depend on id_effect_kit"));
  }
  if !lib_path.is_file() {
    return Err(not_a_project("no src/lib.rs"));
  }
  let found = find_entity(project, entity)?;
  let config = AdminConfig::of(&found.def);

  let builder = config.builder();
  let list = quoted(&config.list);
  let sql = found
    .def
    .krate
    .clone()
    .unwrap_or_else(|| "id_effect_sql".to_owned());
  let vars = [
    ("admin", ADMIN_IMPORT),
    ("entity", entity),
    ("entity_path", found.path.as_str()),
    ("slug", slug.as_str()),
    ("config", builder.as_str()),
    ("list", list.as_str()),
    ("sql", sql.as_str()),
  ];
  let flags = [
    ("read_only", read_only),
    ("filters", !config.filters.is_empty()),
  ];
  let entity_text = render_in("generate/admin", "entity.rs.template", &flags, &vars)?;

  let admin_dir = project.join("src/admin");
  let entity_rel = format!("src/admin/{module}.rs");
  let entity_path = project.join(&entity_rel);
  let mod_path = admin_dir.join("mod.rs");
  let existing_entity = if entity_path.exists() {
    Some(read(&entity_path)?)
  } else {
    None
  };
  if existing_entity
    .as_ref()
    .is_some_and(|text| *text != entity_text)
  {
    return Err(GenerateError::Exists(entity_path));
  }
  let existing_mod = if mod_path.is_file() {
    Some(read(&mod_path)?)
  } else {
    None
  };
  if existing_entity.is_none()
    && existing_mod
      .as_deref()
      .is_some_and(|text| declares_mod(text, &module))
  {
    return Err(GenerateError::AlreadyDeclared {
      module,
      file: mod_path,
    });
  }
  let lib = read(&lib_path)?;

  let mut changes = Vec::new();
  let mut by_hand = Vec::new();
  if existing_entity.is_none() {
    changes.push(Change {
      path: entity_rel,
      created: true,
      what: format!(
        "admin() (EntityAdmin<{entity}>: {}), register, permissions and tests",
        config.summary()
      ),
    });
  }

  // src/admin/mod.rs: the module, its registration and its permissions.
  let mut admin_mod = match &existing_mod {
    Some(text) => text.clone(),
    None => render_in(
      "generate/admin",
      "mod.rs.template",
      &[],
      &[("admin", ADMIN_IMPORT)],
    )?,
  };
  let mut mod_what = Vec::new();
  if existing_mod.is_none() {
    mod_what
      .push("PREFIX, site(), permissions(), policy() and router() (behind a guard)".to_owned());
  }
  if !declares_mod(&admin_mod, &module) {
    admin_mod = insert_mod(&admin_mod, &module);
    mod_what.push(format!("pub mod {module}"));
  }
  for (function, line) in [
    ("site", format!("let site = {module}::register(site)?;")),
    (
      "permissions",
      format!("let permissions = [permissions, {module}::permissions()?].concat();"),
    ),
  ] {
    if admin_mod.contains(&line) {
      continue;
    }
    match insert_before_ok(&admin_mod, function, &line) {
      Some(text) => {
        admin_mod = text;
        mod_what.push(format!("{module} in {function}()"));
      }
      None => by_hand.push(format!("src/admin/mod.rs: add `{line}` to {function}()")),
    }
  }
  let mod_changed = existing_mod.as_deref() != Some(admin_mod.as_str());
  if mod_changed {
    changes.push(Change {
      path: "src/admin/mod.rs".to_owned(),
      created: existing_mod.is_none(),
      what: mod_what.join(", "),
    });
  }

  // src/lib.rs: the `admin` module.
  let new_lib = if declares_mod(&lib, "admin") {
    None
  } else {
    changes.push(Change {
      path: "src/lib.rs".to_owned(),
      created: false,
      what: "pub mod admin".to_owned(),
    });
    Some(insert_mod(&lib, "admin"))
  };

  // Cargo.toml: the dependency, the kit's `authz` feature (and their patches).
  let (new_cargo, cargo_what) = add_admin_dependencies(&cargo, &mut by_hand);
  if !cargo_what.is_empty() {
    changes.push(Change {
      path: "Cargo.toml".to_owned(),
      created: false,
      what: cargo_what.join(", "),
    });
  }

  fs::create_dir_all(&admin_dir).map_err(|source| GenerateError::Io {
    path: admin_dir,
    source,
  })?;
  if existing_entity.is_none() {
    write_new(&entity_path, &entity_text)?;
  }
  if mod_changed {
    if existing_mod.is_some() {
      write(&mod_path, &admin_mod)?;
    } else {
      write_new(&mod_path, &admin_mod)?;
    }
  }
  if let Some(text) = new_lib {
    write(&lib_path, &text)?;
  }
  if new_cargo != cargo {
    write(&cargo_path, &new_cargo)?;
  }
  let actions: &[&str] = if read_only {
    &["list", "view"]
  } else {
    &["list", "view", "create", "update", "delete", "bulk"]
  };
  Ok(AdminReport {
    entity: entity.to_owned(),
    module,
    permissions: actions
      .iter()
      .map(|action| format!("admin.{slug}.{action}"))
      .collect(),
    slug,
    changes,
    by_hand,
  })
}

/// Checks the entity's type name and returns its module name (`DocumentLabel` →
/// `document_label`).
fn validate_entity_name(entity: &str) -> Result<String, GenerateError> {
  let invalid = |reason: String| {
    Err(GenerateError::InvalidEntity {
      entity: entity.to_owned(),
      reason,
    })
  };
  let Some(first) = entity.chars().next() else {
    return invalid("it is empty".to_owned());
  };
  if entity.len() > MAX_ENTITY_NAME {
    return invalid(format!("it is longer than {MAX_ENTITY_NAME} characters"));
  }
  if !first.is_ascii_uppercase() || !entity.chars().all(|c| c.is_ascii_alphanumeric()) {
    return invalid(
      "give the struct's type name in CamelCase (ASCII letters and digits, e.g. `Document`)"
        .to_owned(),
    );
  }
  let module = snake_case(entity);
  if module != "jobs" && RESERVED_NAMES.contains(&module.as_str()) {
    return invalid(format!(
      "its module name `{module}` is a Rust keyword or `tests`"
    ));
  }
  Ok(module)
}

/// `DocumentLabel` → `document_label` (as `#[derive(Entity)]` names an entity).
fn snake_case(name: &str) -> String {
  let mut out = String::new();
  let mut prev_lower = false;
  for c in name.chars() {
    if c.is_ascii_uppercase() {
      if prev_lower {
        out.push('_');
      }
      out.push(c.to_ascii_lowercase());
      prev_lower = false;
    } else {
      out.push(c);
      prev_lower = true;
    }
  }
  out
}

/// The entity name of a `--permissions` prefix: `admin.<name>` (or `<name>`), `<name>`
/// snake_case ASCII starting with a letter, at most 64 bytes (`id_effect_admin`'s rule).
fn permission_slug(prefix: &str) -> Result<String, GenerateError> {
  let invalid = |reason| {
    Err(GenerateError::InvalidPermissions {
      prefix: prefix.to_owned(),
      reason,
    })
  };
  let name = prefix.strip_prefix("admin.").unwrap_or(prefix);
  if name.is_empty() {
    return invalid("it names no entity: give `admin.<name>`");
  }
  if name.len() > MAX_ENTITY_NAME {
    return invalid("the name is longer than 64 characters");
  }
  let snake = name
    .chars()
    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
  if !name.starts_with(|c: char| c.is_ascii_lowercase()) || !snake {
    return invalid(
      "admin permissions are `admin.<name>.<action>`: give `admin.<name>` with a snake_case name \
       (lowercase ASCII letters, digits and `_`, starting with a letter)",
    );
  }
  Ok(name.to_owned())
}

/// `["a", "b"]` → `"a", "b"`.
fn quoted(names: &[String]) -> String {
  names
    .iter()
    .map(|n| format!("\"{n}\""))
    .collect::<Vec<_>>()
    .join(", ")
}

/// Whether a field name looks like it holds a secret (see [`SECRET_WORDS`]).
fn is_secret(name: &str) -> bool {
  let lower = name.to_ascii_lowercase();
  ["api_key", "apikey", "private_key"]
    .iter()
    .any(|s| lower.contains(s))
    || lower.split('_').any(|word| SECRET_WORDS.contains(&word))
}

/// The admin generated for an entity: Rust field names per builder call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AdminConfig {
  list: Vec<String>,
  exclude: Vec<String>,
  search: Vec<String>,
  filters: Vec<String>,
  sortable: Vec<String>,
}

impl AdminConfig {
  /// The configuration of `def` (which has an id field): the id and up to [`MAX_LIST`] more
  /// columns, secrets excluded, `bool` and status-like fields filtered, text fields searched,
  /// non-null columns sortable. The tenant, soft-delete and version columns are shown on the
  /// detail page only.
  fn of(def: &StructDef) -> Self {
    let bookkeeping = |f: &FieldDef| {
      f.version
        || def.tenant.as_deref() == Some(f.column.as_str())
        || def.soft_delete.as_deref() == Some(f.column.as_str())
    };
    let names = |fields: Vec<&FieldDef>| fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
    let exclude = names(
      def
        .fields
        .iter()
        .filter(|f| !f.id && is_secret(&f.name))
        .collect(),
    );
    let shown: Vec<&FieldDef> = def
      .fields
      .iter()
      .filter(|f| f.id || !is_secret(&f.name))
      .collect();
    let columns: Vec<&FieldDef> = shown
      .iter()
      .copied()
      .filter(|f| f.id)
      .chain(
        shown
          .iter()
          .copied()
          .filter(|f| !f.id && !bookkeeping(f) && f.kind != Kind::Other)
          .take(MAX_LIST),
      )
      .collect();
    let filterable = |f: &FieldDef| {
      f.kind == Kind::Bool
        || (f.kind == Kind::Text
          && (FILTER_NAMES.contains(&f.name.as_str()) || f.name.ends_with("_status")))
    };
    let candidates = || shown.iter().copied().filter(|f| !f.id && !bookkeeping(f));
    let filters = names(
      candidates()
        .filter(|f| filterable(f))
        .take(MAX_FILTERS)
        .collect(),
    );
    let search = names(
      candidates()
        .filter(|f| f.kind == Kind::Text && !filters.contains(&f.name))
        .take(MAX_SEARCH)
        .collect(),
    );
    let sortable = names(
      columns
        .iter()
        .copied()
        .filter(|f| !f.nullable && f.kind != Kind::Other && f.kind != Kind::Bool)
        .collect(),
    );
    Self {
      list: names(columns),
      exclude,
      search,
      filters,
      sortable,
    }
  }

  /// The `EntityAdmin` builder calls after `.slug(SLUG)`, one per line (no final newline).
  fn builder(&self) -> String {
    let indent = "        ";
    let mut lines = vec![format!("{indent}.list([{}])", quoted(&self.list))];
    if !self.exclude.is_empty() {
      lines.push(format!("{indent}.exclude([{}])", quoted(&self.exclude)));
    }
    if !self.search.is_empty() {
      lines.push(format!("{indent}.search([{}])", quoted(&self.search)));
    }
    for field in &self.filters {
      lines.push(format!("{indent}.filter(\"{field}\", Filter::exact())"));
    }
    if !self.sortable.is_empty() {
      lines.push(format!("{indent}.sortable([{}])", quoted(&self.sortable)));
    }
    lines.join("\n")
  }

  /// One line for the report: `list id, title; search title; …`.
  fn summary(&self) -> String {
    let mut parts = vec![format!("list {}", self.list.join(", "))];
    for (what, names) in [
      ("search", &self.search),
      ("filters", &self.filters),
      ("excluded", &self.exclude),
    ] {
      if !names.is_empty() {
        parts.push(format!("{what} {}", names.join(", ")));
      }
    }
    parts.join("; ")
  }
}

/// Adds `id_effect_admin` and `id_effect_kit`'s `authz` feature to `cargo`, and the crates they
/// bring to a `[patch.crates-io]` that points id_effect at a checkout. Returns the new text and
/// what changed; a change it cannot make is pushed on `by_hand`.
fn add_admin_dependencies(cargo: &str, by_hand: &mut Vec<String>) -> (String, Vec<String>) {
  let mut lines: Vec<String> = cargo.lines().map(str::to_owned).collect();
  let mut what = Vec::new();
  let version = dependency(cargo, "id_effect")
    .or_else(|| dependency(cargo, "id_effect_kit"))
    .and_then(version_of)
    .unwrap_or(ID_EFFECT_VERSION)
    .to_owned();
  let dep = format!("id_effect_admin = \"{version}\"");
  let current = view(&lines);
  if entry(&current, "dependencies", "id_effect_admin").is_none() {
    if let Some((start, end)) = table(&current, "dependencies") {
      let at = entry(&current, "dependencies", "id_effect")
        .unwrap_or_else(|| last_entry(&current, start, end));
      let comment =
        "# The admin back office (src/admin/, `id-effect generate admin`): guarded entity pages.";
      lines.splice(at + 1..at + 1, [comment.to_owned(), dep.clone()]);
      what.push(dep);
    } else {
      by_hand.push(format!("Cargo.toml: add `{dep}` under [dependencies]"));
    }
  }

  // The kit's policies and route guards, which the generated `policy()` and tests use.
  let current = view(&lines);
  if let Some(i) = entry(&current, "dependencies", "id_effect_kit")
    && !lines[i].contains("\"authz\"")
  {
    match with_feature(&lines[i], "authz") {
      Some(line) => {
        lines[i] = line;
        what.push("id_effect_kit feature `authz`".to_owned());
      }
      None => by_hand.push("Cargo.toml: enable id_effect_kit's `authz` feature".to_owned()),
    }
  }

  add_patches(&mut lines, &ADMIN_CRATES, &mut what, by_hand);
  let mut text = lines.join("\n");
  if cargo.ends_with('\n') {
    text.push('\n');
  }
  (text, what)
}

/// The one-line dependency `line` with `feature` turned on, or `None` when it is not a form this
/// can edit (a multi-line table).
fn with_feature(line: &str, feature: &str) -> Option<String> {
  let line = line.trim_end();
  let (key, value) = line.split_once('=')?;
  let value = value.trim();
  let quoted = format!("\"{feature}\"");
  let new = if value.starts_with('"') && value.ends_with('"') {
    format!(
      "{} = {{ version = {value}, features = [{quoted}] }}",
      key.trim()
    )
  } else if line.contains("features = []") {
    line.replacen("features = []", &format!("features = [{quoted}]"), 1)
  } else if line.contains("features = [") {
    line.replacen("features = [", &format!("features = [{quoted}, "), 1)
  } else {
    let head = line.strip_suffix('}')?.trim_end();
    format!("{head}, features = [{quoted}] }}")
  };
  new.ends_with('}').then_some(new)
}

/// The entity `generate admin` found: its definition, the path naming it from `crate::admin`
/// (`crate::documents::Document`).
#[derive(Debug)]
struct FoundEntity {
  def: StructDef,
  path: String,
}

/// Finds the `#[derive(Entity)]` struct `entity` in the library: `src/lib.rs` and the modules
/// under `src/` (files and inline `mod x { … }`).
fn find_entity(project: &Path, entity: &str) -> Result<FoundEntity, GenerateError> {
  let mut files = Vec::new();
  rust_files(&project.join("src"), &mut files)?;
  files.sort();
  let mut derived = Vec::new();
  let mut plain = Vec::new();
  for path in files {
    let text = read(&path)?;
    let rel = path
      .strip_prefix(project)
      .unwrap_or(&path)
      .to_string_lossy()
      .replace('\\', "/");
    for def in parse_structs(&tokenize(&text)) {
      if def.name != entity {
        continue;
      }
      if def.derives_entity {
        derived.push((rel.clone(), def));
      } else {
        plain.push(rel.clone());
      }
    }
  }
  let invalid = |reason: String| GenerateError::InvalidEntity {
    entity: entity.to_owned(),
    reason,
  };
  let mut derived = derived.into_iter();
  let Some((file, def)) = derived.next() else {
    let reason = match plain.first() {
      Some(file) => format!("`struct {entity}` in {file} does not `#[derive(Entity)]`"),
      None => "no `#[derive(Entity)]` struct of that name in the library (src/)".to_owned(),
    };
    return Err(GenerateError::UnknownEntity {
      entity: entity.to_owned(),
      reason,
    });
  };
  if let Some((other, _)) = derived.next() {
    return Err(invalid(format!(
      "it is declared twice ({file} and {other}): keep one"
    )));
  }
  let Some(mut path) = module_path(&file) else {
    return Err(invalid(format!(
      "it is in {file}, outside the library: move it into src/lib.rs or a module of it"
    )));
  };
  if !def.public {
    return Err(invalid(format!(
      "it is private in {file}: make it `pub` (or `pub(crate)`) so src/admin/ can name it"
    )));
  }
  if def.generic {
    return Err(invalid(
      "it has type parameters: the admin needs a concrete entity type".to_owned(),
    ));
  }
  let Some(id) = def.fields.iter().find(|f| f.id) else {
    return Err(invalid(
      "it has no `#[entity(id)]` field with a name (the primary key)".to_owned(),
    ));
  };
  if matches!(id.kind, Kind::Other | Kind::Float) {
    return Err(invalid(format!(
      "its id `{}` is of a type the admin cannot read from a URL (use an integer, String or \
       Uuid key)",
      id.name
    )));
  }
  path.extend(def.modules.iter().cloned());
  path.push(def.name.clone());
  Ok(FoundEntity {
    path: format!("crate::{}", path.join("::")),
    def,
  })
}

/// The `.rs` files under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), GenerateError> {
  let io = |source| GenerateError::Io {
    path: dir.to_path_buf(),
    source,
  };
  for item in fs::read_dir(dir).map_err(io)? {
    let path = item.map_err(io)?.path();
    if path.is_dir() {
      rust_files(&path, out)?;
    } else if path.extension().is_some_and(|ext| ext == "rs") {
      out.push(path);
    }
  }
  Ok(())
}

/// The module path of the library file `file` (`src/a/b.rs` → `["a", "b"]`, `src/lib.rs` →
/// `[]`); `None` outside the library (`src/main.rs`, `src/bin/`).
fn module_path(file: &str) -> Option<Vec<String>> {
  let parts: Vec<&str> = file.strip_prefix("src/")?.split('/').collect();
  match parts.as_slice() {
    ["lib.rs"] => return Some(Vec::new()),
    ["main.rs"] | ["bin", ..] => return None,
    _ => {}
  }
  let (last, dirs) = parts.split_last()?;
  let mut path: Vec<String> = dirs.iter().map(|d| (*d).to_owned()).collect();
  let stem = last.strip_suffix(".rs")?;
  if stem != "mod" {
    path.push(stem.to_owned());
  }
  Some(path)
}

/// A token of Rust source, as far as `generate admin` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
  /// An identifier or keyword (a raw identifier without its `r#`), or a number.
  Word(String),
  /// The contents of a string literal (escapes as written).
  Str(String),
  /// One punctuation character.
  Punct(char),
}

impl Tok {
  fn is_word(&self, word: &str) -> bool {
    matches!(self, Self::Word(w) if w == word)
  }
}

/// The tokens of `src`, without comments, char literals and lifetimes.
fn tokenize(src: &str) -> Vec<Tok> {
  let chars: Vec<char> = src.chars().collect();
  let len = chars.len();
  let at = |i: usize| chars.get(i).copied();
  let is_ident = |c: char| c.is_alphanumeric() || c == '_';
  let mut out = Vec::new();
  let mut i = 0;
  while i < len {
    let c = chars[i];
    if c.is_whitespace() {
      i += 1;
    } else if c == '/' && at(i + 1) == Some('/') {
      while i < len && chars[i] != '\n' {
        i += 1;
      }
    } else if c == '/' && at(i + 1) == Some('*') {
      let mut depth = 0;
      while i < len {
        if chars[i] == '/' && at(i + 1) == Some('*') {
          depth += 1;
          i += 2;
        } else if chars[i] == '*' && at(i + 1) == Some('/') {
          depth -= 1;
          i += 2;
          if depth == 0 {
            break;
          }
        } else {
          i += 1;
        }
      }
    } else if let Some((text, end)) = raw_string(&chars, i) {
      out.push(Tok::Str(text));
      i = end;
    } else if c == '"' || (c == 'b' && at(i + 1) == Some('"')) {
      let start = if c == 'b' { i + 2 } else { i + 1 };
      let mut j = start;
      while j < len && chars[j] != '"' {
        j += if chars[j] == '\\' { 2 } else { 1 };
      }
      out.push(Tok::Str(chars[start..j.min(len)].iter().collect()));
      i = j + 1;
    } else if c == '\'' {
      if at(i + 1) == Some('\\') {
        // An escaped char literal: '\n', '\'', '\u{..}'.
        let mut j = i + 3;
        while j < len && chars[j] != '\'' {
          j += 1;
        }
        i = j + 1;
      } else if at(i + 2) == Some('\'') {
        i += 3;
      } else {
        // A lifetime or label.
        i += 1;
        while i < len && is_ident(chars[i]) {
          i += 1;
        }
      }
    } else if is_ident(c) {
      let start = i;
      while i < len && is_ident(chars[i]) {
        i += 1;
      }
      let mut word: String = chars[start..i].iter().collect();
      if word == "r" && at(i) == Some('#') && at(i + 1).is_some_and(is_ident) {
        let raw = i + 1;
        i = raw;
        while i < len && is_ident(chars[i]) {
          i += 1;
        }
        word = chars[raw..i].iter().collect();
      }
      out.push(Tok::Word(word));
    } else {
      out.push(Tok::Punct(c));
      i += 1;
    }
  }
  out
}

/// A raw string literal (`r"…"`, `r#"…"#`, `br"…"`) starting at `i`: its contents and the index
/// after it.
fn raw_string(chars: &[char], i: usize) -> Option<(String, usize)> {
  let mut j = i;
  if chars.get(j) == Some(&'b') {
    j += 1;
  }
  if chars.get(j) != Some(&'r') {
    return None;
  }
  j += 1;
  let hashes = chars[j..].iter().take_while(|c| **c == '#').count();
  j += hashes;
  if chars.get(j) != Some(&'"') {
    return None;
  }
  let start = j + 1;
  let mut k = start;
  while k < chars.len() {
    if chars[k] == '"'
      && chars[k + 1..]
        .iter()
        .take(hashes)
        .filter(|c| **c == '#')
        .count()
        == hashes
    {
      return Some((chars[start..k].iter().collect(), k + 1 + hashes));
    }
    k += 1;
  }
  Some((chars[start..].iter().collect(), chars.len()))
}

/// The index of the bracket closing the one at `open` (`(`, `[` or `{`), or `toks.len()` when
/// it is not closed.
fn group_end(toks: &[Tok], open: usize) -> usize {
  let mut depth = 0usize;
  for (i, tok) in toks.iter().enumerate().skip(open) {
    match tok {
      Tok::Punct('(' | '[' | '{') => depth += 1,
      Tok::Punct(')' | ']' | '}') => {
        depth -= 1;
        if depth == 0 {
          return i;
        }
      }
      _ => {}
    }
  }
  toks.len()
}

/// What a value of a field's type is to the admin (`id_effect_admin`'s value kinds, by the last
/// path segment of the type).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
  Bool,
  Int,
  Float,
  Text,
  Uuid,
  Date,
  Timestamp,
  /// JSON, bytes, decimals, `u64`, custom types: shown, never parsed, filtered or sorted.
  Other,
}

impl Kind {
  fn of(segment: &str) -> Self {
    match segment {
      "bool" => Self::Bool,
      "i8" | "i16" | "u8" | "i32" | "u16" | "i64" | "u32" | "isize" => Self::Int,
      "f32" | "f64" => Self::Float,
      "String" | "str" => Self::Text,
      "Uuid" => Self::Uuid,
      "Date" => Self::Date,
      "Timestamp" => Self::Timestamp,
      _ => Self::Other,
    }
  }
}

/// A struct as `generate admin` reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StructDef {
  name: String,
  /// The inline modules (`mod x { … }`) around it in its file.
  modules: Vec<String>,
  /// `pub` or `pub(crate)`.
  public: bool,
  generic: bool,
  derives_entity: bool,
  /// `#[entity(tenant = "..")]`, `soft_delete`, `crate`.
  tenant: Option<String>,
  soft_delete: Option<String>,
  krate: Option<String>,
  /// The named fields (none for a tuple or unit struct).
  fields: Vec<FieldDef>,
}

/// A named field of a [`StructDef`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct FieldDef {
  /// The Rust field name (the admin's name for it).
  name: String,
  /// Its column (`#[from_row(rename)]`, else `#[schema("..")]`, else the name).
  column: String,
  kind: Kind,
  /// `Option<T>`.
  nullable: bool,
  /// `#[entity(id)]`.
  id: bool,
  /// `#[entity(version)]`.
  version: bool,
}

/// The structs declared in `toks` at module level (not inside a function or `impl` body).
fn parse_structs(toks: &[Tok]) -> Vec<StructDef> {
  let mut out = Vec::new();
  // One entry per open `{`: the inline module it opens, or `None` for any other block.
  let mut scopes: Vec<Option<String>> = Vec::new();
  let mut attrs: Vec<&[Tok]> = Vec::new();
  let mut public = false;
  let mut i = 0;
  while i < toks.len() {
    let tok = &toks[i];
    if *tok == Tok::Punct('#') {
      let inner = toks.get(i + 1) == Some(&Tok::Punct('!'));
      let open = if inner { i + 2 } else { i + 1 };
      if toks.get(open) == Some(&Tok::Punct('[')) {
        let close = group_end(toks, open);
        if !inner {
          attrs.push(&toks[open + 1..close]);
        }
        i = close + 1;
        continue;
      }
    } else if tok.is_word("pub") {
      public = true;
      i += 1;
      if toks.get(i) == Some(&Tok::Punct('(')) {
        let close = group_end(toks, i);
        public = matches!(&toks[i + 1..close], [Tok::Word(w)] if w == "crate");
        i = close + 1;
      }
      continue;
    } else if tok.is_word("struct")
      && let Some(Tok::Word(name)) = toks.get(i + 1)
    {
      let (mut def, end) = parse_struct(toks, i + 2);
      if scopes.iter().all(Option::is_some) {
        def.name = name.clone();
        def.public = public;
        def.modules = scopes.iter().flatten().cloned().collect();
        container_attrs(&mut def, &attrs);
        out.push(def);
      }
      attrs.clear();
      public = false;
      i = end;
      continue;
    } else if tok.is_word("mod")
      && let (Some(Tok::Word(name)), Some(Tok::Punct('{'))) = (toks.get(i + 1), toks.get(i + 2))
    {
      scopes.push(Some(name.clone()));
      attrs.clear();
      public = false;
      i += 3;
      continue;
    } else if *tok == Tok::Punct('{') {
      scopes.push(None);
    } else if *tok == Tok::Punct('}') {
      scopes.pop();
    }
    attrs.clear();
    public = false;
    i += 1;
  }
  out
}

/// The struct whose generics or body start at `i`: its fields, and the index after it.
fn parse_struct(toks: &[Tok], mut i: usize) -> (StructDef, usize) {
  let mut def = StructDef::default();
  if toks.get(i) == Some(&Tok::Punct('<')) {
    def.generic = true;
    let mut depth = 0usize;
    while i < toks.len() {
      match toks[i] {
        Tok::Punct('<') => depth += 1,
        Tok::Punct('>') if toks[i - 1] == Tok::Punct('-') => {}
        Tok::Punct('>') => {
          depth -= 1;
          if depth == 0 {
            break;
          }
        }
        _ => {}
      }
      i += 1;
    }
  }
  // Past a `where` clause, to the body.
  while i < toks.len() && !matches!(toks[i], Tok::Punct('{' | '(' | ';')) {
    i += 1;
  }
  match toks.get(i) {
    Some(Tok::Punct('{')) => {
      let close = group_end(toks, i);
      def.fields = parse_fields(&toks[i + 1..close]);
      (def, close + 1)
    }
    Some(Tok::Punct('(')) => (def, group_end(toks, i) + 1),
    _ => (def, i + 1),
  }
}

/// The named fields of a struct body.
fn parse_fields(toks: &[Tok]) -> Vec<FieldDef> {
  let mut fields = Vec::new();
  let mut attrs: Vec<&[Tok]> = Vec::new();
  let mut i = 0;
  while i < toks.len() {
    match &toks[i] {
      Tok::Punct('#') if toks.get(i + 1) == Some(&Tok::Punct('[')) => {
        let close = group_end(toks, i + 1);
        attrs.push(&toks[i + 2..close]);
        i = close + 1;
      }
      Tok::Word(w) if w == "pub" => {
        i += 1;
        if toks.get(i) == Some(&Tok::Punct('(')) {
          i = group_end(toks, i) + 1;
        }
      }
      Tok::Word(name) if toks.get(i + 1) == Some(&Tok::Punct(':')) => {
        let start = i + 2;
        let end = type_end(toks, start);
        fields.push(field_def(name, &toks[start..end], &attrs));
        attrs.clear();
        i = end + 1;
      }
      _ => i += 1,
    }
  }
  fields
}

/// The index of the `,` ending the type starting at `start`, or `toks.len()`.
fn type_end(toks: &[Tok], start: usize) -> usize {
  let mut depth = 0i32;
  for (i, tok) in toks.iter().enumerate().skip(start) {
    match tok {
      Tok::Punct('>') if i > start && toks[i - 1] == Tok::Punct('-') => {}
      Tok::Punct('<' | '(' | '[') => depth += 1,
      Tok::Punct('>' | ')' | ']') => depth -= 1,
      Tok::Punct(',') if depth == 0 => return i,
      _ => {}
    }
  }
  toks.len()
}

/// The field `name: ty` with its attributes.
fn field_def(name: &str, ty: &[Tok], attrs: &[&[Tok]]) -> FieldDef {
  let (segment, args) = last_segment(ty);
  let (kind, nullable) = match args {
    Some(inner) if segment == "Option" => (Kind::of(last_segment(inner).0), true),
    _ => (Kind::of(segment), false),
  };
  let mut field = FieldDef {
    name: name.to_owned(),
    column: name.to_owned(),
    kind,
    nullable,
    id: false,
    version: false,
  };
  let (mut rename, mut schema) = (None, None);
  for attr in attrs {
    let (head, items) = attr_items(attr);
    for (key, value) in items {
      match (head, key.as_str(), value) {
        ("entity", "id", None) => field.id = true,
        ("entity", "version", None) => field.version = true,
        ("from_row", "rename", Some(column)) => rename = Some(column),
        ("schema", "", Some(column)) => schema = Some(column),
        _ => {}
      }
    }
  }
  if let Some(column) = rename.or(schema) {
    field.column = column;
  }
  field
}

/// The struct attributes `generate admin` reads: `#[derive(.. Entity ..)]` and
/// `#[entity(tenant = "..", soft_delete = "..", crate = path)]`.
fn container_attrs(def: &mut StructDef, attrs: &[&[Tok]]) {
  for attr in attrs {
    let (head, items) = attr_items(attr);
    if head == "derive" {
      def.derives_entity |= attr.iter().any(|t| t.is_word("Entity"));
    }
    if head != "entity" {
      continue;
    }
    for (key, value) in items {
      match key.as_str() {
        "tenant" => def.tenant = value,
        "soft_delete" => def.soft_delete = value,
        "crate" => def.krate = value,
        _ => {}
      }
    }
  }
}

/// The last path segment of a type and its generic arguments (`jiff::Timestamp` →
/// `("Timestamp", None)`, `Option<String>` → `("Option", Some(String))`); `""` for a tuple,
/// array or slice type.
fn last_segment(ty: &[Tok]) -> (&str, Option<&[Tok]>) {
  let open = ty.iter().position(|t| *t == Tok::Punct('<'));
  let head = &ty[..open.unwrap_or(ty.len())];
  if head.iter().any(|t| matches!(t, Tok::Punct('(' | '['))) {
    return ("", None);
  }
  let segment = head
    .iter()
    .rev()
    .find_map(|t| match t {
      Tok::Word(w) => Some(w.as_str()),
      _ => None,
    })
    .unwrap_or("");
  let args = open
    .filter(|_| ty.last() == Some(&Tok::Punct('>')))
    .map(|o| &ty[o + 1..ty.len() - 1]);
  (segment, args)
}

/// An attribute's name and its `key`, `key = value` and `"literal"` items (`""` key): `entity(id,
/// label = "x")` → `("entity", [("id", None), ("label", Some("x"))])`. Values are string
/// contents or paths as written (`::id_effect_kit::sql`).
fn attr_items(attr: &[Tok]) -> (&str, Vec<(String, Option<String>)>) {
  let head = match attr.first() {
    Some(Tok::Word(w)) => w.as_str(),
    _ => "",
  };
  let mut items = Vec::new();
  if attr.get(1) != Some(&Tok::Punct('(')) {
    return (head, items);
  }
  let close = group_end(attr, 1);
  let mut start = 2;
  while start < close {
    let mut end = start;
    let mut depth = 0i32;
    while end < close {
      match attr[end] {
        Tok::Punct('(' | '[' | '{') => depth += 1,
        Tok::Punct(')' | ']' | '}') => depth -= 1,
        Tok::Punct(',') if depth == 0 => break,
        _ => {}
      }
      end += 1;
    }
    match &attr[start..end] {
      [Tok::Word(key)] => items.push((key.clone(), None)),
      [Tok::Str(value)] => items.push((String::new(), Some(value.clone()))),
      [Tok::Word(key), Tok::Punct('='), rest @ ..] if !rest.is_empty() => {
        let value = rest
          .iter()
          .map(|t| match t {
            Tok::Word(w) | Tok::Str(w) => w.clone(),
            Tok::Punct(c) => c.to_string(),
          })
          .collect();
        items.push((key.clone(), Some(value)));
      }
      _ => {}
    }
    start = end + 1;
  }
  (head, items)
}

#[cfg(test)]
mod tests {
  use super::*;

  mod names {
    use super::*;

    #[test]
    fn a_snake_case_name_gives_its_camel_case_type() {
      for (name, ty) in [
        ("send_report", "SendReport"),
        ("x", "X"),
        ("sync2_db", "Sync2Db"),
      ] {
        assert_eq!(validate_job_name(name).unwrap(), ty);
      }
    }

    #[test]
    fn bad_names_are_refused_with_the_reason() {
      let long = "a".repeat(MAX_JOB_NAME + 1);
      for name in [
        "",
        "9lives",
        "_x",
        "SendReport",
        "send-report",
        "send__report",
        "report_",
        "fn",
        "tests",
        "jobs",
        "job",
        "worker",
        "schema",
        long.as_str(),
      ] {
        assert!(
          matches!(
            validate_job_name(name),
            Err(GenerateError::InvalidName { .. })
          ),
          "{name}"
        );
      }
    }

    #[test]
    fn queues_and_cron_expressions_are_checked() {
      assert!(validate_queue("mail.high-1").is_ok());
      for queue in ["", "a b", "q\""] {
        assert!(validate_queue(queue).is_err(), "{queue}");
      }
      assert!(validate_cron("0 6 * * *").is_ok());
      for expr in ["every day", "0 6 * *", "0 6 * * \"x"] {
        assert!(
          matches!(validate_cron(expr), Err(GenerateError::InvalidCron { .. })),
          "{expr}"
        );
      }
    }
  }

  mod edits {
    use super::*;

    #[test]
    fn a_module_goes_into_the_sorted_run_of_declarations() {
      let text = "//! Doc.\n\nuse a::b;\n\npub mod alpha;\npub mod omega;\n\nfn f() {}\n";
      assert_eq!(
        insert_mod(text, "jobs"),
        "//! Doc.\n\nuse a::b;\n\npub mod alpha;\npub mod jobs;\npub mod omega;\n\nfn f() {}\n"
      );
      assert_eq!(
        insert_mod("mod a;\n", "z"),
        "mod a;\npub mod z;\n",
        "appended to the run"
      );
    }

    #[test]
    fn without_declarations_a_module_follows_the_inner_docs() {
      let text = "//! Doc.\n//! More.\n\nuse a::b;\n";
      assert_eq!(
        insert_mod(text, "jobs"),
        "//! Doc.\n//! More.\n\npub mod jobs;\n\nuse a::b;\n"
      );
      assert_eq!(
        insert_mod("fn f() {}\n", "jobs"),
        "pub mod jobs;\n\nfn f() {}\n"
      );
    }

    #[test]
    fn declared_modules_are_found_in_every_form() {
      for text in [
        "mod jobs;\n",
        "pub mod jobs;\n",
        "pub(crate) mod jobs {\n}\n",
      ] {
        assert!(declares_mod(text, "jobs"), "{text}");
      }
      assert!(!declares_mod("pub mod jobs_old;\n    mod jobs;\n", "jobs"));
    }

    #[test]
    fn a_registration_goes_before_the_closing_ok() {
      let text = "pub fn registry() -> R {\n    let registry = new();\n    Ok(registry)\n}\n";
      assert_eq!(
        insert_before_ok(
          text,
          "registry",
          "let registry = registry.register(a::handle)?;"
        )
        .unwrap(),
        "pub fn registry() -> R {\n    let registry = new();\n    let registry = registry.register(a::handle)?;\n    Ok(registry)\n}\n"
      );
      let edited = "pub fn registry() -> R {\n    build()\n}\nfn other() { Ok(registry) }\n";
      assert_eq!(insert_before_ok(edited, "registry", "x"), None);
    }

    const CARGO: &str = "[package]\nname = \"orders-api\"\n\n[dependencies]\nid_effect_kit = { version = \"0.9.0\", features = [\"web\"] }\nid_effect = \"0.9.0\"\naxum = \"0.8\"\n\n[dev-dependencies]\ntower = \"0.5\"\n";

    #[test]
    fn the_dependency_follows_id_effect_at_its_version() {
      let mut by_hand = Vec::new();
      let (text, what) = add_jobs_dependency(CARGO, false, &mut by_hand);
      assert!(
        text.contains("id_effect = \"0.9.0\"\n# Background jobs"),
        "{text}"
      );
      assert!(
        text.contains("\nid_effect_jobs = \"0.9.0\"\naxum"),
        "{text}"
      );
      assert_eq!(what, ["id_effect_jobs = \"0.9.0\""]);
      assert!(by_hand.is_empty());
      let (again, what) = add_jobs_dependency(&text, false, &mut by_hand);
      assert_eq!((again, what.len()), (text, 0), "idempotent");
    }

    #[test]
    fn cron_turns_the_feature_on_in_every_dependency_form() {
      let mut by_hand = Vec::new();
      let (text, _) = add_jobs_dependency(CARGO, true, &mut by_hand);
      assert!(
        text.contains("id_effect_jobs = { version = \"0.9.0\", features = [\"cron\"] }"),
        "{text}"
      );
      for (line, want) in [
        (
          "id_effect_jobs = \"0.9\"",
          "id_effect_jobs = { version = \"0.9\", features = [\"cron\"] }",
        ),
        (
          "id_effect_jobs = { version = \"0.9\" }",
          "id_effect_jobs = { version = \"0.9\", features = [\"cron\"] }",
        ),
        (
          "id_effect_jobs = { version = \"0.9\", features = [] }",
          "id_effect_jobs = { version = \"0.9\", features = [\"cron\"] }",
        ),
        (
          "id_effect_jobs = { version = \"0.9\", features = [\"x\"] }",
          "id_effect_jobs = { version = \"0.9\", features = [\"cron\", \"x\"] }",
        ),
      ] {
        let cargo = format!("[dependencies]\nid_effect = \"0.9\"\n{line}\n");
        let (text, _) = add_jobs_dependency(&cargo, true, &mut by_hand);
        assert!(text.contains(want), "{line} -> {text}");
      }
      assert!(by_hand.is_empty(), "{by_hand:?}");
      let multi = "[dependencies]\nid_effect_jobs = { version = \"0.9\", features = [\n] }\n";
      add_jobs_dependency(multi, true, &mut by_hand);
      assert_eq!(by_hand.len(), 1, "a multi-line entry is left alone");
    }

    #[test]
    fn a_local_workspace_patch_gets_the_new_crates() {
      let cargo = format!(
        "{CARGO}\n# --local-workspace\n[patch.crates-io]\nid_effect = {{ path = \"/ws/crates/id_effect\" }}\nid_effect_sql = {{ path = \"/ws/crates/id_effect_sql\" }}\n"
      );
      let mut by_hand = Vec::new();
      let (text, what) = add_jobs_dependency(&cargo, true, &mut by_hand);
      for name in ["id_effect_jobs", "id_effect_node", "id_effect_process"] {
        let want = format!("\n{name} = {{ path = \"/ws/crates/{name}\" }}\n");
        assert!(text.contains(&want), "{name}: {text}");
      }
      assert_eq!(text.matches("crates/id_effect_sql").count(), 1, "{text}");
      assert!(what.iter().any(|w| w.starts_with("[patch.crates-io]")));
    }
  }

  #[test]
  fn every_job_template_renders_with_and_without_cron() {
    let vars = [
      ("job", "send_report"),
      ("job_type", "SendReport"),
      ("crate_name", "orders_api"),
      ("queue", "mail"),
      ("cron", "0 6 * * *"),
    ];
    for cron in [false, true] {
      let job = render("job.rs.template", cron, &vars).unwrap();
      assert!(job.contains("impl Job for SendReport {"), "{job}");
      assert!(job.contains("const NAME: &'static str = \"orders_api.send_report\";"));
      assert!(job.contains("const QUEUE: &'static str = \"mail\";"));
      assert_eq!(job.contains("pub const CRON: &str = \"0 6 * * *\";"), cron);
      assert!(!job.contains("{{"), "{job}");
    }
    let jobs_mod = render("mod.rs.template", false, &vars).unwrap();
    assert!(jobs_mod.contains("pub fn registry()"));
    assert!(
      render("schedule.rs.template", false, &vars)
        .unwrap()
        .contains("pub fn schedule()")
    );
  }

  mod failures {
    use std::error::Error as _;

    use super::*;

    const CARGO: &str =
      "[package]\nname = \"orders-api\"\n\n[dependencies]\nid_effect = \"0.9.0\"\n";

    fn project(cargo: &str, lib: Option<&str>) -> tempfile::TempDir {
      let tmp = tempfile::tempdir().unwrap();
      fs::write(tmp.path().join("Cargo.toml"), cargo).unwrap();
      if let Some(lib) = lib {
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/lib.rs"), lib).unwrap();
      }
      tmp
    }

    #[test]
    fn a_project_without_id_effect_or_a_library_is_refused() {
      let plain = project(
        "[package]\nname = \"p\"\n\n[dependencies]\nserde = \"1\"\n",
        None,
      );
      let err = generate_job(plain.path(), "send", "default", None).unwrap_err();
      assert!(
        matches!(
          err,
          GenerateError::NotAProject {
            reason: "Cargo.toml does not depend on id_effect",
            ..
          }
        ),
        "{err}"
      );
      assert!(err.to_string().contains("is not an id_effect project"));
      let no_lib = project(CARGO, None);
      assert!(matches!(
        generate_job(no_lib.path(), "send", "default", None),
        Err(GenerateError::NotAProject {
          reason: "no src/lib.rs",
          ..
        })
      ));
    }

    #[test]
    fn a_jobs_path_that_is_a_file_fails_with_io() {
      let tmp = project(CARGO, Some("pub fn f() {}\n"));
      fs::write(tmp.path().join("src/jobs"), "not a directory").unwrap();
      let err = generate_job(tmp.path(), "send", "default", None).unwrap_err();
      let GenerateError::Io { path, .. } = &err else {
        panic!("expected an io error, got {err}");
      };
      assert!(path.ends_with("src/jobs"), "{}", path.display());
      assert!(err.source().is_some());
    }

    #[test]
    fn an_existing_schedule_without_ok_is_left_to_the_user() {
      let tmp = project(CARGO, Some("pub mod jobs;\n"));
      let jobs = "pub fn registry() -> R {\n    let registry = new();\n    Ok(registry)\n}\n\npub fn schedule() -> S {\n    build()\n}\n";
      fs::create_dir_all(tmp.path().join("src/jobs")).unwrap();
      fs::write(tmp.path().join("src/jobs/mod.rs"), jobs).unwrap();
      let report = generate_job(tmp.path(), "nightly", "default", Some("0 6 * * *")).unwrap();
      assert!(
        report
          .by_hand
          .iter()
          .any(|l| l.contains("nightly::cron()") && l.contains("schedule()")),
        "{:?}",
        report.by_hand
      );
      let written = fs::read_to_string(tmp.path().join("src/jobs/mod.rs")).unwrap();
      assert_eq!(written.matches("pub fn schedule(").count(), 1, "{written}");
      assert!(written.contains("registry.register(nightly::handle)"));
      assert_eq!(report.job_name, "orders_api.nightly");
    }

    #[test]
    fn long_queues_and_unknown_templates_are_errors() {
      assert!(matches!(
        validate_queue(&"q".repeat(65)),
        Err(GenerateError::InvalidQueue {
          reason: "it is longer than 64 characters",
          ..
        })
      ));
      let err = render("missing.template", false, &[]).unwrap_err();
      assert_eq!(
        err.to_string(),
        "template generate/job/missing.template: not embedded"
      );
      assert!(err.source().is_none());
    }

    #[test]
    fn file_helpers_report_the_failing_path() {
      let tmp = tempfile::tempdir().unwrap();
      let missing = tmp.path().join("missing.rs");
      assert!(matches!(read(&missing), Err(GenerateError::Io { .. })));
      assert!(matches!(
        write(tmp.path(), "x"),
        Err(GenerateError::Io { .. })
      ));
      let existing = tmp.path().join("there.rs");
      fs::write(&existing, "").unwrap();
      let err = write_new(&existing, "x").unwrap_err();
      assert!(err.to_string().starts_with(&existing.display().to_string()));
    }

    #[test]
    fn new_errors_convert_into_generate_errors() {
      let template = GenerateError::from(NewError::Template {
        file: "f".into(),
        reason: "r".into(),
      });
      assert!(matches!(template, GenerateError::Template { ref file, .. } if file == "f"));
      let io = GenerateError::from(NewError::Io {
        path: PathBuf::from("/p"),
        source: io::Error::other("disk"),
      });
      assert_eq!(io.to_string(), "/p: disk");
      let other = GenerateError::from(NewError::Exists(PathBuf::from("/d")));
      let GenerateError::Template { file, reason } = other else {
        panic!("folded into a template error");
      };
      assert_eq!(file, "generate/job");
      assert!(reason.contains("/d"), "{reason}");
    }

    #[test]
    fn insert_mod_terminates_an_unterminated_last_line() {
      assert_eq!(insert_mod("mod a;", "z"), "mod a;\npub mod z;\n");
    }

    #[test]
    fn versions_are_read_from_tables_too() {
      assert_eq!(
        version_of("id_effect = { version = \"0.9.1\", default-features = false }"),
        Some("0.9.1")
      );
      assert_eq!(version_of("id_effect = { path = \"../x\" }"), None);
      assert_eq!(version_of("id_effect"), None);
    }

    #[test]
    fn missing_dependency_table_and_unpatchable_workspace_go_by_hand() {
      let mut by_hand = Vec::new();
      let (_, what) = add_jobs_dependency("[package]\nname = \"p\"\n", false, &mut by_hand);
      assert!(what.is_empty());
      assert_eq!(
        by_hand,
        ["Cargo.toml: add `id_effect_jobs = \"".to_owned()
          + ID_EFFECT_VERSION
          + "\"` under [dependencies]"]
      );
      let cargo = "[dependencies]\nid_effect = \"0.9\"\n\n[patch.crates-io]\nid_effect = { git = \"https://example.invalid/id_effect\" }\n";
      let mut by_hand = Vec::new();
      add_jobs_dependency(cargo, false, &mut by_hand);
      assert_eq!(by_hand.len(), 1, "{by_hand:?}");
      assert!(
        by_hand[0].starts_with("Cargo.toml: patch id_effect_jobs"),
        "{by_hand:?}"
      );
    }
  }

  mod admin {
    use super::*;

    fn words(src: &str) -> Vec<Tok> {
      tokenize(src)
    }

    fn word(w: &str) -> Tok {
      Tok::Word(w.to_owned())
    }

    fn structs(src: &str) -> Vec<StructDef> {
      parse_structs(&tokenize(src))
    }

    fn one(src: &str) -> StructDef {
      let mut all = structs(src);
      assert_eq!(all.len(), 1, "{all:?}");
      all.remove(0)
    }

    #[test]
    fn comments_literals_and_lifetimes_are_skipped() {
      let toks = words(
        "// line\n/* outer /* inner */ still */ a 'x' '\\n' '\\u{1F600}' &'static b\n\"s\\\"q\" b\"by\" r#\"raw \" here\"# br\"b\" r#type",
      );
      assert_eq!(
        toks,
        [
          word("a"),
          Tok::Punct('&'),
          word("b"),
          Tok::Str("s\\\"q".into()),
          Tok::Str("by".into()),
          Tok::Str("raw \" here".into()),
          Tok::Str("b".into()),
          word("type"),
        ]
      );
      assert_eq!(words("r#\"open"), [Tok::Str("open".into())]);
      assert_eq!(words("\"open"), [Tok::Str("open".into())]);
      assert_eq!(words("r#x"), [word("x")]);
      assert_eq!(words("r#"), [word("r"), Tok::Punct('#')]);
      assert_eq!(group_end(&words("( a"), 0), 2, "unclosed");
    }

    #[test]
    fn structs_are_found_with_their_module_and_visibility() {
      let src = "#![allow(x)]\n#[derive(Entity)] pub struct A { #[entity(id)] id: i64 }\n\
                 mod inner { #[derive(Clone, sql::Entity)] pub(crate) struct B { pub(crate) x: u8, } }\n\
                 pub(super) struct C;\nstruct D(i64);\nfn f() { pub struct Hidden { a: i64 } }\n\
                 impl X { }\n# not_an_attr\npub struct G<T: Fn() -> u8> where T: Copy { t: T }";
      let all = structs(src);
      let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
      assert_eq!(names, ["A", "B", "C", "D", "G"]);
      assert!(all[0].derives_entity && all[0].public && all[0].modules.is_empty());
      assert!(all[1].derives_entity && all[1].public);
      assert_eq!(all[1].modules, ["inner"]);
      assert_eq!(all[1].fields[0].kind, Kind::Int);
      assert!(!all[2].public && !all[2].derives_entity);
      assert!(all[3].fields.is_empty() && !all[3].public);
      assert!(all[4].generic);
      assert_eq!(all[4].fields[0].name, "t");
      assert!(structs("struct").is_empty());
    }

    #[test]
    fn fields_read_their_column_kind_and_role() {
      let def = one(
        "#[derive(Entity)]\n#[entity(table = \"t\", tenant = \"org\", soft_delete = \"gone\", crate = ::id_effect_kit::sql, belongs_to(X, name = \"x\"))]\n\
         pub struct T {\n\
           #[entity(id, generated)] pub id: uuid::Uuid,\n\
           #[from_row(rename = \"org\")] pub org_id: i64,\n\
           #[schema(\"title_col\")] title: Option<String>,\n\
           #[entity(version)] version: i32,\n\
           when: Option<jiff::Timestamp>, day: Date, score: f64, flag: bool,\n\
           pair: (i64, i64), bytes: [u8; 4], map: HashMap<String, i64>, f: fn(i64) -> i64,\n\
           #[doc = \"x\"] text: &'static str, opt: Option<Vec<u8>>, odd: Option<>\n\
         }",
      );
      assert_eq!(def.tenant.as_deref(), Some("org"));
      assert_eq!(def.soft_delete.as_deref(), Some("gone"));
      assert_eq!(def.krate.as_deref(), Some("::id_effect_kit::sql"));
      let get = |n: &str| def.fields.iter().find(|f| f.name == n).unwrap().clone();
      assert!(get("id").id && get("id").kind == Kind::Uuid);
      assert_eq!(get("org_id").column, "org");
      assert_eq!(
        (
          get("title").column.as_str(),
          get("title").kind,
          get("title").nullable
        ),
        ("title_col", Kind::Text, true)
      );
      assert!(get("version").version);
      assert_eq!(
        (
          get("when").kind,
          get("day").kind,
          get("score").kind,
          get("flag").kind
        ),
        (Kind::Timestamp, Kind::Date, Kind::Float, Kind::Bool)
      );
      for other in ["pair", "bytes", "map", "f", "opt", "odd"] {
        assert_eq!(get(other).kind, Kind::Other, "{other}");
      }
      assert_eq!(get("text").kind, Kind::Text);
      assert_eq!(def.fields.len(), 15);
    }

    #[test]
    fn library_files_map_to_module_paths() {
      assert_eq!(module_path("src/lib.rs"), Some(vec![]));
      assert_eq!(module_path("src/a.rs"), Some(vec!["a".to_owned()]));
      assert_eq!(
        module_path("src/a/b/mod.rs"),
        Some(vec!["a".to_owned(), "b".to_owned()])
      );
      for outside in ["src/main.rs", "src/bin/x.rs", "tests/x.rs", "src/a.txt"] {
        assert_eq!(module_path(outside), None, "{outside}");
      }
    }

    #[test]
    fn entity_names_and_permission_prefixes_are_checked() {
      assert_eq!(
        validate_entity_name("DocumentLabel").unwrap(),
        "document_label"
      );
      assert_eq!(validate_entity_name("Jobs").unwrap(), "jobs");
      assert_eq!(validate_entity_name("V2Item").unwrap(), "v2_item");
      let long = format!("A{}", "b".repeat(MAX_ENTITY_NAME));
      for bad in ["", "document", "Doc_Item", "Type", "Tests", long.as_str()] {
        assert!(
          matches!(
            validate_entity_name(bad),
            Err(GenerateError::InvalidEntity { .. })
          ),
          "{bad}"
        );
      }
      assert_eq!(permission_slug("admin.docs").unwrap(), "docs");
      assert_eq!(permission_slug("docs_2").unwrap(), "docs_2");
      let long = format!("admin.{}", "a".repeat(MAX_ENTITY_NAME + 1));
      for bad in [
        "admin.",
        "",
        "admin.Docs",
        "admin.2x",
        "admin.a.b",
        long.as_str(),
      ] {
        let err = permission_slug(bad).unwrap_err();
        assert!(
          err.to_string().starts_with("invalid --permissions"),
          "{bad}: {err}"
        );
      }
    }

    #[test]
    fn secrets_are_recognized_by_word() {
      for secret in [
        "password_hash",
        "api_token",
        "apiKey",
        "stripe_api_key",
        "totp",
        "secret",
      ] {
        assert!(is_secret(secret), "{secret}");
      }
      for plain in ["footprint", "tokenizer_name", "title", "keyword"] {
        assert!(!is_secret(plain), "{plain}");
      }
    }

    fn field(name: &str, kind: Kind) -> FieldDef {
      FieldDef {
        name: name.to_owned(),
        column: name.to_owned(),
        kind,
        nullable: false,
        id: false,
        version: false,
      }
    }

    #[test]
    fn the_configuration_follows_the_fields() {
      let mut id = field("key", Kind::Text);
      id.id = true;
      let mut note = field("note", Kind::Text);
      note.nullable = true;
      let def = StructDef {
        fields: vec![
          id,
          field("blob", Kind::Other),
          note,
          field("a", Kind::Bool),
          field("b", Kind::Bool),
          field("c", Kind::Bool),
          field("order_status", Kind::Text),
          field("d", Kind::Bool),
          field("e", Kind::Int),
        ],
        ..StructDef::default()
      };
      let config = AdminConfig::of(&def);
      assert_eq!(config.list, ["key", "note", "a", "b", "c", "order_status"]);
      assert_eq!(config.filters, ["a", "b", "c", "order_status"]);
      assert_eq!(config.search, ["note"]);
      assert_eq!(config.sortable, ["key", "order_status"]);
      assert!(
        config
          .builder()
          .contains(".filter(\"order_status\", Filter::exact())")
      );

      let mut only_id = field("id", Kind::Bool);
      only_id.id = true;
      let bare = AdminConfig::of(&StructDef {
        fields: vec![only_id],
        ..StructDef::default()
      });
      assert_eq!(bare.builder(), "        .list([\"id\"])");
      assert_eq!(bare.summary(), "list id");
    }

    #[test]
    fn features_are_added_to_every_one_line_form() {
      for (line, want) in [
        (
          "k = \"1\"",
          "k = { version = \"1\", features = [\"authz\"] }",
        ),
        (
          "k = { version = \"1\" }",
          "k = { version = \"1\", features = [\"authz\"] }",
        ),
        (
          "k = { version = \"1\", features = [] }",
          "k = { version = \"1\", features = [\"authz\"] }",
        ),
        (
          "k = { version = \"1\", features = [\"web\"] }",
          "k = { version = \"1\", features = [\"authz\", \"web\"] }",
        ),
      ] {
        assert_eq!(with_feature(line, "authz").as_deref(), Some(want), "{line}");
      }
      assert_eq!(
        with_feature("k = { version = \"1\", features = [", "authz"),
        None
      );
      assert_eq!(with_feature("k", "authz"), None);
    }

    #[test]
    fn admin_dependencies_follow_the_project() {
      let mut by_hand = Vec::new();
      let (text, what) = add_admin_dependencies("[package]\nname = \"p\"\n", &mut by_hand);
      assert_eq!(text, "[package]\nname = \"p\"\n");
      assert!(what.is_empty());
      assert_eq!(by_hand.len(), 1, "{by_hand:?}");

      let cargo = "[dependencies]\nid_effect_kit = { version = \"0.9.1\", features = [\n] }\n\n# --local-workspace\n[patch.crates-io]\nid_effect = { path = \"/ws/crates/id_effect\" }\nid_effect_sql = { path = \"/ws/crates/id_effect_sql\" }";
      let mut by_hand = Vec::new();
      let (text, what) = add_admin_dependencies(cargo, &mut by_hand);
      assert!(text.contains("\nid_effect_admin = \"0.9.1\"\n"), "{text}");
      assert_eq!(
        by_hand,
        ["Cargo.toml: enable id_effect_kit's `authz` feature"]
      );
      for name in ["id_effect_admin", "id_effect_authz", "id_effect_axum"] {
        assert!(
          text.contains(&format!("\n{name} = {{ path = \"/ws/crates/{name}\" }}")),
          "{text}"
        );
      }
      assert_eq!(text.matches("crates/id_effect_sql").count(), 1);
      assert!(!text.ends_with('\n'));
      assert_eq!(what.len(), 2, "{what:?}");
      let (again, what) = add_admin_dependencies(&text, &mut Vec::new());
      assert_eq!((again, what.len()), (text, 0), "idempotent");
    }

    const CARGO: &str = "[package]\nname = \"shop\"\n\n[dependencies]\nid_effect_kit = { version = \"0.9.0\", features = [\"web\"] }\n";
    const ENTITY: &str = "#[derive(Entity)]\npub struct Doc {\n    #[entity(id)]\n    pub id: i64,\n    pub title: String,\n}\n";

    fn shop(files: &[(&str, &str)]) -> tempfile::TempDir {
      let tmp = tempfile::tempdir().unwrap();
      fs::write(tmp.path().join("Cargo.toml"), CARGO).unwrap();
      fs::create_dir_all(tmp.path().join("src")).unwrap();
      fs::write(tmp.path().join("src/lib.rs"), "pub mod docs;\n").unwrap();
      for (path, text) in files {
        let path = tmp.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
      }
      tmp
    }

    fn reason(result: Result<AdminReport, GenerateError>) -> String {
      result.unwrap_err().to_string()
    }

    #[test]
    fn entities_the_admin_cannot_name_are_refused() {
      for (files, want) in [
        (
          vec![("src/a.rs", ENTITY), ("src/b.rs", ENTITY)],
          "it is declared twice (src/a.rs and src/b.rs)",
        ),
        (vec![("src/main.rs", ENTITY)], "outside the library"),
        (
          vec![(
            "src/docs.rs",
            "#[derive(Entity)]\nstruct Doc { #[entity(id)] id: i64 }\n",
          )],
          "it is private in src/docs.rs",
        ),
        (
          vec![(
            "src/docs.rs",
            "#[derive(Entity)]\npub struct Doc<T> { #[entity(id)] id: T }\n",
          )],
          "it has type parameters",
        ),
        (
          vec![(
            "src/docs.rs",
            "#[derive(Entity)]\npub struct Doc { id: i64 }\n",
          )],
          "it has no `#[entity(id)]` field",
        ),
        (
          vec![(
            "src/docs.rs",
            "#[derive(Entity)]\npub struct Doc { #[entity(id)] id: u64 }\n",
          )],
          "its id `id` is of a type the admin cannot read",
        ),
      ] {
        let tmp = shop(&files);
        let err = reason(generate_admin(tmp.path(), "Doc", false, None));
        assert!(
          err.starts_with("invalid entity `Doc`: ") && err.contains(want),
          "{err}"
        );
      }
    }

    #[test]
    fn a_project_without_the_kit_or_a_library_is_refused() {
      let tmp = shop(&[]);
      fs::write(
        tmp.path().join("Cargo.toml"),
        "[dependencies]\nid_effect = \"1\"\n",
      )
      .unwrap();
      let err = reason(generate_admin(tmp.path(), "Doc", false, None));
      assert!(err.contains("does not depend on id_effect_kit"), "{err}");
      fs::write(tmp.path().join("Cargo.toml"), CARGO).unwrap();
      fs::remove_file(tmp.path().join("src/lib.rs")).unwrap();
      let err = reason(generate_admin(tmp.path(), "Doc", false, None));
      assert!(err.contains("(no src/lib.rs)"), "{err}");
    }

    #[test]
    fn a_module_declared_by_hand_is_not_shadowed() {
      let tmp = shop(&[
        ("src/docs.rs", ENTITY),
        ("src/admin/mod.rs", "mod doc {}\n"),
      ]);
      let err = generate_admin(tmp.path(), "Doc", false, None).unwrap_err();
      assert!(
        matches!(&err, GenerateError::AlreadyDeclared { module, .. } if module == "doc"),
        "{err}"
      );
    }

    #[test]
    fn an_edited_admin_mod_goes_by_hand() {
      let tmp = shop(&[
        ("src/docs.rs", ENTITY),
        (
          "src/admin/mod.rs",
          "//! Mine.\n\npub fn site() -> S {\n    build()\n}\n",
        ),
      ]);
      let report = generate_admin(tmp.path(), "Doc", true, Some("admin.paper")).unwrap();
      assert_eq!(report.by_hand.len(), 2, "{:?}", report.by_hand);
      assert_eq!(report.slug, "paper");
      assert_eq!(report.permissions, ["admin.paper.list", "admin.paper.view"]);
      let text = admin_report_text(&report);
      assert!(
        text.contains("Do by hand:\n  src/admin/mod.rs: add `let site = doc::register(site)?;`"),
        "{text}"
      );
      let admin = fs::read_to_string(tmp.path().join("src/admin/mod.rs")).unwrap();
      assert!(admin.starts_with("//! Mine.\n\npub mod doc;\n"), "{admin}");
      let doc = fs::read_to_string(tmp.path().join("src/admin/doc.rs")).unwrap();
      assert!(doc.contains("pub const SLUG: &str = \"paper\";"), "{doc}");
      assert!(
        doc.contains("&[AdminAction::List, AdminAction::View]"),
        "{doc}"
      );
      assert!(!doc.contains("Filter"), "{doc}");
      assert!(doc.contains("use crate::docs::Doc;"), "{doc}");

      let again = generate_admin(tmp.path(), "Doc", true, Some("admin.paper")).unwrap();
      assert_eq!(again.by_hand.len(), 2);
      assert!(again.changes.is_empty(), "{:?}", again.changes);
      assert!(admin_report_text(&again).contains("up to date: nothing was written"));
    }

    #[test]
    fn unreadable_paths_fail_with_io() {
      let tmp = shop(&[("src/docs.rs", ENTITY), ("src/admin", "a file")]);
      let err = generate_admin(tmp.path(), "Doc", false, None).unwrap_err();
      assert!(matches!(err, GenerateError::Io { .. }), "{err}");
      let tmp = shop(&[("src/docs.rs", ENTITY)]);
      fs::create_dir_all(tmp.path().join("src/admin/doc.rs")).unwrap();
      let err = generate_admin(tmp.path(), "Doc", false, None).unwrap_err();
      assert!(matches!(err, GenerateError::Io { .. }), "{err}");
      assert!(matches!(
        rust_files(&tmp.path().join("missing"), &mut Vec::new()),
        Err(GenerateError::Io { .. })
      ));
      let err = render_in("generate/admin", "missing.template", &[], &[]).unwrap_err();
      assert_eq!(
        err.to_string(),
        "template generate/admin/missing.template: not embedded"
      );
    }
  }
}
