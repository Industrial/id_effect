//! `id-effect generate`: add code to a project `id-effect new` generated.
//!
//! ```text
//! id-effect generate job <NAME> [--queue QUEUE] [--cron EXPR] [--path DIR]
//! ```
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
  /// Add a background job (id_effect_jobs): src/jobs/<NAME>.rs with its payload, `Job` impl,
  /// handler and a test, registered in src/jobs/mod.rs; adds the id_effect_jobs dependency.
  Job(JobArgs),
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
  let label = format!("generate/job/{source}");
  let file = FILES
    .iter()
    .find(|f| f.dir == "generate/job" && f.source == source)
    .ok_or_else(|| GenerateError::Template {
      file: label.clone(),
      reason: "not embedded".to_owned(),
    })?;
  let lookup = |flag: &str| (flag == "cron").then_some(cron);
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
  let current = view(&lines);
  if let Some((start, end)) = table(&current, "patch.crates-io") {
    let root = entry(&current, "patch.crates-io", "id_effect").and_then(|i| {
      let path = current[i]
        .split_once("path")?
        .1
        .trim_start()
        .strip_prefix('=')?;
      let path = path.trim_start().strip_prefix('"')?.split_once('"')?.0;
      path.strip_suffix("/crates/id_effect").map(str::to_owned)
    });
    let wanted = JOBS_CRATES
      .iter()
      .chain(if cron { &CRON_CRATES[..] } else { &[] })
      .filter(|name| entry(&current, "patch.crates-io", name).is_none())
      .copied()
      .collect::<Vec<_>>();
    if !wanted.is_empty() {
      match root {
        Some(root) => {
          let at = last_entry(&current, start, end) + 1;
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
  }

  let mut text = lines.join("\n");
  if cargo.ends_with('\n') {
    text.push('\n');
  }
  (text, what)
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
}
