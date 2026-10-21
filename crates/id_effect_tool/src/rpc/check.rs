//! `id-effect rpc check`: breaking-change detection between two service contracts (design note
//! `r12-rpc-design` §1, `web.rpc#6`), and a staleness check of a checked-in TypeScript client.
//!
//! ```text
//! id-effect rpc check --contract rpc.contract.json --against v0.11.0:crates/api/rpc.contract.json
//! id-effect rpc check --contract rpc.contract.json --generated web/src/rpc
//! ```
//!
//! [`compare`] classifies every change between the previous contract (`--against`: a file, or
//! `<git-ref>:<path>` read with `git cat-file` in the directory of `--contract`) and the current
//! one. A procedure is only compared with the same `(service, version)`:
//!
//! | Class | Changes |
//! |---|---|
//! | [`Class::Breaking`] | a procedure or a served version removed (a rename is a removal); `kind` changed; `access` tightened (public to guarded, a policy added); a required input field added or an optional one made required; an input type narrowed (a type, value or alternative removed, a bound tightened, unknown fields refused); an output field removed or made optional; an output type widened (a type, value or alternative added, a bound loosened); an error variant added; `idempotent` dropped; `max_items` / `max_bytes` lowered |
//! | [`Class::Compatible`] | a procedure, service or version added; an optional input field added; an input type widened; an output field added; an output type narrowed; an error variant removed; access loosened; `idempotent` added; a cap raised; `deprecated` set; a definition renamed |
//! | [`Class::Versioned`] | a breaking change made in a *new* version of a service, compared with the newest older version: allowed (`web.rpc#6`), provided that older version is still served — when it is not, its removal is itself a breaking change |
//!
//! Any other change of a schema keyword (`allOf`, a `pattern` or `multipleOf` replaced, a tuple's
//! length) is breaking in both directions. Descriptions are ignored.
//!
//! **Definitions.** `$defs` keys are Rust `type_name` paths, which change when a module moves. A
//! `$ref` is therefore followed into the `$defs` of its own schema root on each side and the two
//! bodies are compared structurally; the key never takes part. Where a name is shown (a renamed
//! definition, a union alternative) it is the OpenAPI component name
//! ([`id_effect_rpc::openapi`], also used by `gen ts`): the last `::` segment of the key, or, when
//! two keys of one root share it, the shortest `::` suffix no other key shares, joined with `.`.
//! A moved module changes nothing; a renamed type is reported as compatible (the wire shape is the
//! same, generated type names change). Union alternatives are matched by their `_tag` const when
//! they have one, else by their normalized text (references rewritten to those names); a single
//! unmatched alternative on each side is compared structurally.
//!
//! **Error unions** are the `oneOf` of `{"_tag": {"const": …}}` objects; variants are matched by
//! tag and their fields compared like outputs. `error: null` is the empty union.
//!
//! **Caps.** An absent `max_items` / `max_bytes` is the server default (10,000 items, 4 MiB).
//!
//! **Retiring a version.** `--allow-retired Service:N` (repeatable) declares the removal of that
//! service version intended: its `version no longer served` change is reported as `retired` and
//! passes ([`retire`]). It excuses nothing else, and naming a version that was not removed is a
//! usage error, so a typo cannot widen the check.
//!
//! `--generated <dir>` regenerates the TypeScript client in memory ([`gen_ts::generate`]) and
//! fails when a file in `dir` is missing or differs ([`stale_files`]).
//!
//! Exit codes: `0` no breaking change and the generated client is current, `1` a breaking change
//! within a published version or a stale client, `2` a usage error or an unreadable contract
//! (either side).
//!
//! ```rust
//! use id_effect_rpc::contract::{Contract, ProcedureAccess, ProcedureContract, ProcedureKind, ServiceContract};
//! use id_effect_tool::rpc::check::{Class, compare};
//! use serde_json::json;
//!
//! let greet = |input| ProcedureContract::new("greet", ProcedureKind::Query, ProcedureAccess::Public {},
//!   input, json!({"type": "string"}));
//! let v1 = |input| Contract::new().with_service(ServiceContract::new("Greeter", 1).with_procedure(greet(input)));
//! let old = v1(json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}));
//! let new = v1(json!({"type": "object", "properties": {"name": {"type": "string"}, "polite": {"type": "boolean"}},
//!   "required": ["name", "polite"]}));
//! let changes = compare(&old, &new);
//! assert_eq!(changes.len(), 1);
//! assert_eq!(changes[0].class, Class::Breaking);
//! assert_eq!(changes[0].to_string(), "breaking   Greeter v1 greet input.polite: required field added");
//! ```

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::Args;
use id_effect_rpc::contract::{
  Contract, ContractError, ProcedureAccess, ProcedureContract, ProcedureKind, ServiceContract,
};
use serde_json::Value;

use super::RetiredVersion;
use super::gen_ts::{self, GenTsError};

/// The reason of the [`Class::Breaking`] change for a service version the current contract no
/// longer serves (the change `--allow-retired` excuses).
const VERSION_REMOVED: &str = "version no longer served";

/// The server's client-stream item cap when the contract names none.
pub const DEFAULT_MAX_ITEMS: u64 = 10_000;

/// The server's client-stream byte cap when the contract names none.
pub const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Arguments of `id-effect rpc check`.
#[derive(Debug, Args)]
pub struct CheckArgs {
  /// The current service contract (`rpc.contract.json`).
  #[arg(long, value_name = "FILE")]
  pub contract: PathBuf,
  /// The previous contract: a file, or `<git-ref>:<path>` (path from the repository root, or
  /// `./…` from the directory of `--contract`).
  #[arg(
    long,
    value_name = "REF:PATH|FILE",
    required_unless_present = "generated"
  )]
  pub against: Option<String>,
  /// Also fail when the TypeScript client in this directory is not what `id-effect rpc gen ts`
  /// writes for `--contract`.
  #[arg(long, value_name = "DIR")]
  pub generated: Option<PathBuf>,
  /// A service version whose removal is intended (`Service:N`, repeatable): its `version no
  /// longer served` change passes as `retired`. Every other breaking change still fails; naming a
  /// version that was not removed is a usage error.
  #[arg(long, value_name = "SERVICE:N", requires = "against")]
  pub allow_retired: Vec<RetiredVersion>,
}

/// How a change affects clients and servers of a published version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
  /// Breaks clients (or servers) of a version that is already published.
  Breaking,
  /// A breaking change in a new version, while the older version is still served.
  Versioned,
  /// Old clients keep working.
  Compatible,
}

impl Class {
  fn as_str(self) -> &'static str {
    match self {
      Self::Breaking => "breaking",
      Self::Versioned => "versioned",
      Self::Compatible => "compatible",
    }
  }
}

impl fmt::Display for Class {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// One classified change between two contracts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
  /// Its class.
  pub class: Class,
  /// The service.
  pub service: String,
  /// The service version (of the current contract for a new version).
  pub version: u32,
  /// The procedure, unless the change concerns the whole service version.
  pub procedure: Option<String>,
  /// Where in the procedure: `input.title`, `output[]`, `error(NotFound).id`, or empty.
  pub path: String,
  /// Why it is classified so.
  pub reason: String,
}

impl fmt::Display for Change {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{:<10} {} v{}",
      self.class.as_str(),
      self.service,
      self.version
    )?;
    if let Some(procedure) = &self.procedure {
      write!(f, " {procedure}")?;
    }
    if !self.path.is_empty() {
      write!(f, " {}", self.path)?;
    }
    write!(f, ": {}", self.reason)
  }
}

/// Whether `changes` may ship: no [`Class::Breaking`] change.
pub fn passes(changes: &[Change]) -> bool {
  changes.iter().all(|c| c.class != Class::Breaking)
}

/// Take the removals of the service versions in `allowed` (`--allow-retired`) out of `changes`
/// and return them: each is the [`Class::Breaking`] `version no longer served` change of that
/// service version. Nothing else is excused, so [`passes`] still fails on any other breaking
/// change.
///
/// Fails, leaving `changes` untouched, with the first entry of `allowed` that names no removed
/// version (a typo, a version still served, or one that never existed).
///
/// ```rust
/// use id_effect_rpc::contract::{Contract, ServiceContract};
/// use id_effect_tool::rpc::RetiredVersion;
/// use id_effect_tool::rpc::check::{compare, passes, retire};
///
/// let old = Contract::new()
///   .with_service(ServiceContract::new("Greeter", 1))
///   .with_service(ServiceContract::new("Greeter", 2));
/// let new = Contract::new().with_service(ServiceContract::new("Greeter", 2));
/// let mut changes = compare(&old, &new);
/// assert!(!passes(&changes));
///
/// let retired = retire(&mut changes, &[RetiredVersion::new("Greeter", 1)]).expect("v1 was removed");
/// assert_eq!(retired.len(), 1);
/// assert!(passes(&changes));
/// assert_eq!(
///   retire(&mut changes, &[RetiredVersion::new("Greeter", 2)]),
///   Err(RetiredVersion::new("Greeter", 2))
/// );
/// ```
pub fn retire(
  changes: &mut Vec<Change>,
  allowed: &[RetiredVersion],
) -> Result<Vec<Change>, RetiredVersion> {
  let removes = |c: &Change, r: &RetiredVersion| {
    c.class == Class::Breaking
      && c.procedure.is_none()
      && c.reason == VERSION_REMOVED
      && c.service == r.service
      && c.version == r.version
  };
  if let Some(unknown) = allowed
    .iter()
    .find(|r| !changes.iter().any(|c| removes(c, r)))
  {
    return Err(unknown.clone());
  }
  let (retired, kept) = std::mem::take(changes)
    .into_iter()
    .partition(|c| allowed.iter().any(|r| removes(c, r)));
  *changes = kept;
  Ok(retired)
}

/// Why a contract could not be read.
#[derive(Debug)]
#[non_exhaustive]
pub enum CheckError {
  /// A file could not be read.
  Read {
    /// The file.
    path: PathBuf,
    /// Why.
    source: std::io::Error,
  },
  /// `git` could not run or refused `<ref>:<path>`.
  Git {
    /// The `--against` value.
    spec: String,
    /// What git said.
    message: String,
  },
  /// The text is not a contract document.
  Contract {
    /// Which side (a path or `<ref>:<path>`).
    what: String,
    /// Why.
    source: ContractError,
  },
}

impl fmt::Display for CheckError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
      Self::Git { spec, message } => write!(f, "cannot read `{spec}` from git: {message}"),
      Self::Contract { what, source } => write!(f, "{what}: {source}"),
    }
  }
}

impl std::error::Error for CheckError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::Read { source, .. } => Some(source),
      Self::Contract { source, .. } => Some(source),
      Self::Git { .. } => None,
    }
  }
}

/// Read the contract file at `path`.
pub fn read_contract(path: &Path) -> Result<Contract, CheckError> {
  let text = std::fs::read_to_string(path).map_err(|source| CheckError::Read {
    path: path.to_path_buf(),
    source,
  })?;
  Contract::from_json(&text).map_err(|source| CheckError::Contract {
    what: path.display().to_string(),
    source,
  })
}

/// Read the previous contract named by `spec`: a file when one exists at `spec`, else
/// `<git-ref>:<path>`, read with `git cat-file blob` run in `dir`.
pub fn read_against(spec: &str, dir: &Path) -> Result<Contract, CheckError> {
  let as_file = Path::new(spec);
  let git = spec
    .split_once(':')
    .filter(|(rev, path)| !rev.is_empty() && !path.is_empty());
  let (rev, path) = match git {
    Some(parts) if !as_file.exists() => parts,
    _ => return read_contract(as_file),
  };
  let git_error = |message: String| CheckError::Git {
    spec: spec.to_owned(),
    message,
  };
  if rev.starts_with('-') {
    return Err(git_error("a git ref cannot start with `-`".to_owned()));
  }
  let output = Command::new("git")
    .arg("cat-file")
    .arg("blob")
    .arg(format!("{rev}:{path}"))
    .current_dir(dir)
    .output()
    .map_err(|e| git_error(e.to_string()))?;
  if !output.status.success() {
    return Err(git_error(
      String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    ));
  }
  let text = String::from_utf8(output.stdout).map_err(|e| git_error(e.to_string()))?;
  Contract::from_json(&text).map_err(|source| CheckError::Contract {
    what: spec.to_owned(),
    source,
  })
}

/// The files of the TypeScript client for `contract` that are missing from `dir` or differ from
/// what `id-effect rpc gen ts` writes.
pub fn stale_files(contract: &Contract, dir: &Path) -> Result<Vec<PathBuf>, GenTsError> {
  Ok(
    gen_ts::generate(contract)?
      .into_iter()
      .map(|file| (dir.join(file.path), file.contents))
      .filter(|(path, contents)| std::fs::read_to_string(path).ok().as_ref() != Some(contents))
      .map(|(path, _)| path)
      .collect(),
  )
}

/// Runs `id-effect rpc check`.
pub fn run(args: &CheckArgs, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
  let unreadable = |err: &mut dyn Write, e: CheckError| {
    let _ = writeln!(err, "error: {e}");
    ExitCode::from(2)
  };
  let current = match read_contract(&args.contract) {
    Ok(c) => c,
    Err(e) => return unreadable(err, e),
  };
  let mut failed = false;
  if let Some(spec) = &args.against {
    let dir = match args.contract.parent() {
      Some(p) if !p.as_os_str().is_empty() => p,
      _ => Path::new("."),
    };
    let previous = match read_against(spec, dir) {
      Ok(c) => c,
      Err(e) => return unreadable(err, e),
    };
    let mut changes = compare(&previous, &current);
    let retired = match retire(&mut changes, &args.allow_retired) {
      Ok(retired) => retired,
      Err(r) => {
        let _ = writeln!(
          err,
          "error: --allow-retired {r}: {} v{} was not removed (it is still served, or {spec} \
           never served it)",
          r.service, r.version
        );
        return ExitCode::from(2);
      }
    };
    let _ = writeln!(out, "rpc check: {} against {spec}", args.contract.display());
    let mut lines: Vec<_> = changes
      .iter()
      .map(|c| (c, c.to_string()))
      .chain(retired.iter().map(|c| {
        let line = format!(
          "{:<10} {} v{}: {} (--allow-retired)",
          "retired", c.service, c.version, c.reason
        );
        (c, line)
      }))
      .collect();
    lines.sort_by(|(a, _), (b, _)| {
      (&a.service, a.version, &a.procedure).cmp(&(&b.service, b.version, &b.procedure))
    });
    for (_, line) in &lines {
      let _ = writeln!(out, "{line}");
    }
    let count = |class| changes.iter().filter(|c| c.class == class).count();
    let breaking = count(Class::Breaking);
    let retired_count = if retired.is_empty() {
      String::new()
    } else {
      format!(" {} retired,", retired.len())
    };
    let _ = writeln!(
      out,
      "{} changes: {breaking} breaking,{retired_count} {} versioned, {} compatible",
      lines.len(),
      count(Class::Versioned),
      count(Class::Compatible)
    );
    if !passes(&changes) {
      failed = true;
      let _ = writeln!(
        err,
        "error: {breaking} breaking change(s) within a published version: bump the service \
         version and keep serving the previous one"
      );
    }
  }
  if let Some(dir) = &args.generated {
    match stale_files(&current, dir) {
      Ok(stale) if stale.is_empty() => {
        let _ = writeln!(out, "generated client {} is current", dir.display());
      }
      Ok(stale) => {
        failed = true;
        for path in &stale {
          let _ = writeln!(out, "stale {}", path.display());
        }
        let _ = writeln!(
          err,
          "error: the generated client is stale: run `id-effect rpc gen ts --contract {} --out {}`",
          args.contract.display(),
          dir.display()
        );
      }
      Err(e) => {
        failed = true;
        let _ = writeln!(err, "error: {e}");
      }
    }
  }
  if failed {
    ExitCode::FAILURE
  } else {
    ExitCode::SUCCESS
  }
}

// ---------------------------------------------------------------------------------------------
// Contracts
// ---------------------------------------------------------------------------------------------

/// Every change from `previous` to `current`, ordered by service, version and procedure.
pub fn compare(previous: &Contract, current: &Contract) -> Vec<Change> {
  let mut out = Vec::new();
  for old in &previous.services {
    match current.service(&old.name, old.version) {
      Some(new) => service(old, new, None, &mut out),
      None => out.push(Change {
        class: Class::Breaking,
        service: old.name.clone(),
        version: old.version,
        procedure: None,
        path: String::new(),
        reason: VERSION_REMOVED.to_owned(),
      }),
    }
  }
  for new in &current.services {
    if previous.service(&new.name, new.version).is_some() {
      continue;
    }
    let base = previous
      .services
      .iter()
      .filter(|s| s.name == new.name && s.version < new.version)
      .max_by_key(|s| s.version);
    let reason = match base {
      Some(base) => format!("version added (compared with v{})", base.version),
      None => "version added".to_owned(),
    };
    out.push(Change {
      class: Class::Compatible,
      service: new.name.clone(),
      version: new.version,
      procedure: None,
      path: String::new(),
      reason,
    });
    if let Some(base) = base {
      let served = current.service(&base.name, base.version).is_some();
      service(base, new, Some(served), &mut out);
    }
  }
  out.sort_by(|a, b| {
    (&a.service, a.version, &a.procedure).cmp(&(&b.service, b.version, &b.procedure))
  });
  out
}

/// Compare two versions of a service. `bump` is `Some(base still served)` when `new` is a new
/// version compared with an older one: its breaking changes are then [`Class::Versioned`].
fn service(
  old: &ServiceContract,
  new: &ServiceContract,
  bump: Option<bool>,
  out: &mut Vec<Change>,
) {
  let mut push = |procedure: &str, path: String, class: Class, reason: String| {
    let (class, reason) = match (bump, class) {
      (Some(served), Class::Breaking) => (
        Class::Versioned,
        format!(
          "{reason} (new version; v{} {})",
          old.version,
          if served {
            "is still served"
          } else {
            "is no longer served"
          }
        ),
      ),
      _ => (class, reason),
    };
    out.push(Change {
      class,
      service: new.name.clone(),
      version: new.version,
      procedure: Some(procedure.to_owned()),
      path,
      reason,
    });
  };
  for p in &old.procedures {
    match new.procedure(&p.name) {
      Some(n) => {
        for (path, class, reason) in procedure(p, n) {
          push(&p.name, path, class, reason);
        }
      }
      None => push(
        &p.name,
        String::new(),
        Class::Breaking,
        "procedure removed".to_owned(),
      ),
    }
  }
  for n in &new.procedures {
    if old.procedure(&n.name).is_none() {
      push(
        &n.name,
        String::new(),
        Class::Compatible,
        "procedure added".to_owned(),
      );
    }
  }
}

/// A change inside one procedure: its path, class and reason.
type Finding = (String, Class, String);

fn procedure(old: &ProcedureContract, new: &ProcedureContract) -> Vec<Finding> {
  let mut out: Vec<Finding> = Vec::new();
  let mut top = |class: Class, reason: String| out.push((String::new(), class, reason));
  if old.kind != new.kind {
    top(
      Class::Breaking,
      format!("kind changed from `{}` to `{}`", old.kind, new.kind),
    );
    return out;
  }
  access(&old.access, &new.access, &mut top);
  if old.idempotent && !new.idempotent {
    top(
      Class::Breaking,
      "no longer idempotent (an Idempotency-Key retry would run it again)".to_owned(),
    );
  } else if !old.idempotent && new.idempotent {
    top(Class::Compatible, "now idempotent".to_owned());
  }
  if old.kind == ProcedureKind::ClientStream || old.kind == ProcedureKind::Bidi {
    cap(
      "max_items",
      old.max_items,
      new.max_items,
      DEFAULT_MAX_ITEMS,
      &mut top,
    );
    cap(
      "max_bytes",
      old.max_bytes,
      new.max_bytes,
      DEFAULT_MAX_BYTES,
      &mut top,
    );
  }
  if !old.deprecated && new.deprecated {
    top(Class::Compatible, "deprecated".to_owned());
  } else if old.deprecated && !new.deprecated {
    top(Class::Compatible, "no longer deprecated".to_owned());
  }
  Cmp::new(&old.input, &new.input, Dir::Input).run("input", &mut out);
  Cmp::new(&old.output, &new.output, Dir::Output).run("output", &mut out);
  let none = Value::Null;
  let old_error = old.error.as_ref().unwrap_or(&none);
  let new_error = new.error.as_ref().unwrap_or(&none);
  Cmp::new(old_error, new_error, Dir::Output).errors(&mut out);
  out
}

fn access(old: &ProcedureAccess, new: &ProcedureAccess, top: &mut impl FnMut(Class, String)) {
  match (old, new) {
    (ProcedureAccess::Public {}, ProcedureAccess::Guarded { policies }) => top(
      Class::Breaking,
      format!(
        "access tightened: public, now guarded by {}",
        list(policies)
      ),
    ),
    (ProcedureAccess::Guarded { .. }, ProcedureAccess::Public {}) => {
      top(Class::Compatible, "access loosened: now public".to_owned());
    }
    (ProcedureAccess::Guarded { policies: o }, ProcedureAccess::Guarded { policies: n }) => {
      let (o, n): (BTreeSet<_>, BTreeSet<_>) = (o.iter().collect(), n.iter().collect());
      for added in n.difference(&o) {
        top(
          Class::Breaking,
          format!("access tightened: policy `{added}` added"),
        );
      }
      for removed in o.difference(&n) {
        top(
          Class::Compatible,
          format!("access loosened: policy `{removed}` removed"),
        );
      }
    }
    (ProcedureAccess::Public {}, ProcedureAccess::Public {}) => {}
  }
}

fn list(items: &[String]) -> String {
  items
    .iter()
    .map(|p| format!("`{p}`"))
    .collect::<Vec<_>>()
    .join(", ")
}

fn cap(
  name: &str,
  old: Option<u64>,
  new: Option<u64>,
  default: u64,
  top: &mut impl FnMut(Class, String),
) {
  let (o, n) = (old.unwrap_or(default), new.unwrap_or(default));
  if n < o {
    top(Class::Breaking, format!("{name} lowered from {o} to {n}"));
  } else if n > o {
    top(Class::Compatible, format!("{name} raised from {o} to {n}"));
  }
}

// ---------------------------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------------------------

/// Which way values flow: inputs from old clients must still be accepted (a narrower schema
/// breaks them), outputs must still be readable by old clients (a wider schema breaks them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
  Input,
  Output,
}

/// What a schema change does to the set of values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
  Narrows,
  Widens,
  /// Neither a subset nor a superset.
  Changes,
  /// Compatible whatever the direction.
  Neutral,
}

static ANY: Value = Value::Bool(true);

/// Keywords that never change what a schema accepts.
const IGNORED: [&str; 10] = [
  "$schema",
  "$defs",
  "$id",
  "$comment",
  "description",
  "title",
  "examples",
  "default",
  "readOnly",
  "writeOnly",
];

/// Keywords [`Cmp`] compares one by one; any other keyword is compared as normalized text.
const HANDLED: [&str; 24] = [
  "$ref",
  "type",
  "const",
  "enum",
  "anyOf",
  "oneOf",
  "properties",
  "required",
  "additionalProperties",
  "items",
  "prefixItems",
  "minimum",
  "exclusiveMinimum",
  "minLength",
  "minItems",
  "minProperties",
  "maximum",
  "exclusiveMaximum",
  "maxLength",
  "maxItems",
  "maxProperties",
  "pattern",
  "format",
  "uniqueItems",
];

/// Bounds where a larger value accepts less.
const LOWER: [&str; 5] = [
  "minimum",
  "exclusiveMinimum",
  "minLength",
  "minItems",
  "minProperties",
];

/// Bounds where a larger value accepts more.
const UPPER: [&str; 5] = [
  "maximum",
  "exclusiveMaximum",
  "maxLength",
  "maxItems",
  "maxProperties",
];

const OLD: usize = 0;
const NEW: usize = 1;

type Map = serde_json::Map<String, Value>;

/// The comparison of one old schema root with one new one.
struct Cmp<'a> {
  roots: [&'a Value; 2],
  names: [BTreeMap<String, String>; 2],
  dir: Dir,
  seen: HashSet<(String, String)>,
  found: Vec<Finding>,
}

impl<'a> Cmp<'a> {
  fn new(old: &'a Value, new: &'a Value, dir: Dir) -> Self {
    Self {
      roots: [old, new],
      names: [def_names(old), def_names(new)],
      dir,
      seen: HashSet::new(),
      found: Vec::new(),
    }
  }

  fn run(mut self, path: &str, out: &mut Vec<Finding>) {
    let [old, new] = self.roots;
    self.schema(old, new, path);
    out.append(&mut self.found);
  }

  fn push(&mut self, path: &str, effect: Effect, reason: String) {
    let class = match (self.dir, effect) {
      (_, Effect::Changes) | (Dir::Input, Effect::Narrows) | (Dir::Output, Effect::Widens) => {
        Class::Breaking
      }
      _ => Class::Compatible,
    };
    self.found.push((path.to_owned(), class, reason));
  }

  /// `schema` with its references followed in `side`'s root, and the last definition key.
  fn deref(&self, side: usize, mut schema: &'a Value) -> (&'a Value, Option<String>) {
    let mut key = None;
    for _ in 0..32 {
      let Some(token) = schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/$defs/"))
      else {
        break;
      };
      let k = unescape(token);
      let Some(body) = self.roots[side].get("$defs").and_then(|d| d.get(&k)) else {
        break;
      };
      schema = body;
      key = Some(k);
    }
    (schema, key)
  }

  fn name(&self, side: usize, key: &str) -> String {
    self.names[side]
      .get(key)
      .cloned()
      .unwrap_or_else(|| key.to_owned())
  }

  /// `schema` as JSON text: keys sorted, annotations dropped, references by definition name.
  fn text(&self, side: usize, schema: &Value) -> String {
    serde_json::to_string(&self.normalize(side, schema)).unwrap_or_default()
  }

  fn normalize(&self, side: usize, schema: &Value) -> Value {
    match schema {
      Value::Object(map) => {
        let sorted: BTreeMap<&String, Value> = map
          .iter()
          .filter(|(k, _)| !IGNORED.contains(&k.as_str()))
          .map(|(k, v)| {
            let token = v.as_str().and_then(|r| r.strip_prefix("#/$defs/"));
            let v = match (k.as_str(), token) {
              ("$ref", Some(token)) => {
                Value::String(format!("#/$defs/{}", self.name(side, &unescape(token))))
              }
              _ => self.normalize(side, v),
            };
            (k, v)
          })
          .collect();
        Value::Object(sorted.into_iter().map(|(k, v)| (k.clone(), v)).collect())
      }
      Value::Array(items) => Value::Array(items.iter().map(|v| self.normalize(side, v)).collect()),
      other => other.clone(),
    }
  }

  fn schema(&mut self, old: &'a Value, new: &'a Value, path: &str) {
    let (old, old_key) = self.deref(OLD, old);
    let (new, new_key) = self.deref(NEW, new);
    if let (Some(o), Some(n)) = (&old_key, &new_key) {
      if !self.seen.insert((o.clone(), n.clone())) {
        return;
      }
      let (on, nn) = (self.name(OLD, o), self.name(NEW, n));
      if on != nn {
        self.push(
          path,
          Effect::Neutral,
          format!("type `{on}` renamed to `{nn}` (same wire shape; generated type names change)"),
        );
      }
    }
    match (old, new) {
      (Value::Bool(false), Value::Bool(false)) => return,
      (Value::Bool(false), _) => {
        return self.push(path, Effect::Widens, "now accepts values".to_owned());
      }
      (_, Value::Bool(false)) => {
        return self.push(path, Effect::Narrows, "now refuses every value".to_owned());
      }
      _ => {}
    }
    let (o, n) = (map_of(old), map_of(new));
    if is_union(o) || is_union(n) {
      return self.union(arms(old), arms(new), path);
    }
    self.types(o, n, path);
    self.literals(o, n, path);
    self.bounds(o, n, path);
    self.object(o, n, path);
    self.array(o, n, path);
    self.rest(o, n, path);
  }

  fn union(&mut self, old: Vec<&'a Value>, new: Vec<&'a Value>, path: &str) {
    let key = |cmp: &Self, side: usize, arm: &'a Value| match tag_of(cmp.deref(side, arm).0) {
      Some(tag) => format!("tag:{tag}"),
      None => cmp.text(side, arm),
    };
    let old: Vec<(String, &'a Value)> = old.into_iter().map(|a| (key(self, OLD, a), a)).collect();
    let mut new: Vec<(String, &'a Value)> =
      new.into_iter().map(|a| (key(self, NEW, a), a)).collect();
    let mut removed = Vec::new();
    for (k, o) in old {
      match new.iter().position(|(nk, _)| *nk == k) {
        Some(i) => {
          let (_, n) = new.remove(i);
          let at = match k.strip_prefix("tag:") {
            Some(tag) => format!("{path}({tag})"),
            None => path.to_owned(),
          };
          self.schema(o, n, &at);
        }
        None => removed.push(o),
      }
    }
    if let ([o], [(_, n)]) = (removed.as_slice(), new.as_slice()) {
      return self.schema(o, n, path);
    }
    for o in removed {
      let what = self.describe(OLD, o);
      self.push(path, Effect::Narrows, format!("alternative {what} removed"));
    }
    for (_, n) in new {
      let what = self.describe(NEW, n);
      self.push(path, Effect::Widens, format!("alternative {what} added"));
    }
  }

  /// A short name for a union alternative.
  fn describe(&self, side: usize, arm: &Value) -> String {
    if let Some(key) = arm
      .get("$ref")
      .and_then(Value::as_str)
      .and_then(|r| r.strip_prefix("#/$defs/"))
    {
      return format!("`{}`", self.name(side, &unescape(key)));
    }
    if let Some(tag) = tag_of(arm) {
      return format!("`{tag}`");
    }
    let text = self.text(side, arm);
    if text.chars().count() > 60 {
      format!("`{}…`", text.chars().take(60).collect::<String>())
    } else {
      format!("`{text}`")
    }
  }

  fn types(&mut self, o: &Map, n: &Map, path: &str) {
    let set = |m: &Map| -> Option<BTreeSet<String>> {
      match m.get("type")? {
        Value::String(t) => Some(BTreeSet::from([t.clone()])),
        Value::Array(ts) => Some(
          ts.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        ),
        _ => None,
      }
    };
    let covers =
      |s: &BTreeSet<String>, t: &str| s.contains(t) || (t == "integer" && s.contains("number"));
    match (set(o), set(n)) {
      (None, Some(n)) => self.push(
        path,
        Effect::Narrows,
        format!("restricted to type {}", types_text(&n)),
      ),
      (Some(o), None) => self.push(
        path,
        Effect::Widens,
        format!("no longer restricted to type {}", types_text(&o)),
      ),
      (Some(o), Some(n)) => {
        for t in o.iter().filter(|t| !covers(&n, t)) {
          let reason = if t == "number" && n.contains("integer") {
            "type `number` narrowed to `integer`".to_owned()
          } else {
            format!("type `{t}` removed")
          };
          self.push(path, Effect::Narrows, reason);
        }
        for t in n.iter().filter(|t| !covers(&o, t)) {
          let reason = if t == "number" && o.contains("integer") {
            "type `integer` widened to `number`".to_owned()
          } else {
            format!("type `{t}` added")
          };
          self.push(path, Effect::Widens, reason);
        }
      }
      (None, None) => {}
    }
  }

  fn literals(&mut self, o: &Map, n: &Map, path: &str) {
    let values = |m: &Map| -> Option<Vec<Value>> {
      if let Some(v) = m.get("const") {
        return Some(vec![v.clone()]);
      }
      m.get("enum").and_then(Value::as_array).cloned()
    };
    match (values(o), values(n)) {
      (None, Some(n)) => self.push(
        path,
        Effect::Narrows,
        format!("restricted to {}", values_text(&n)),
      ),
      (Some(o), None) => self.push(
        path,
        Effect::Widens,
        format!("no longer restricted to {}", values_text(&o)),
      ),
      (Some(o), Some(n)) => {
        for v in o.iter().filter(|v| !n.contains(v)) {
          self.push(path, Effect::Narrows, format!("value `{v}` removed"));
        }
        for v in n.iter().filter(|v| !o.contains(v)) {
          self.push(path, Effect::Widens, format!("value `{v}` added"));
        }
      }
      (None, None) => {}
    }
  }

  fn bounds(&mut self, o: &Map, n: &Map, path: &str) {
    for (keys, tighter_when_larger) in [(LOWER, true), (UPPER, false)] {
      for key in keys {
        let a = o.get(key).and_then(Value::as_f64);
        let b = n.get(key).and_then(Value::as_f64);
        let effect = match (a, b) {
          (None, Some(_)) => Effect::Narrows,
          (Some(_), None) => Effect::Widens,
          (Some(a), Some(b)) if a != b && (b > a) == tighter_when_larger => Effect::Narrows,
          (Some(a), Some(b)) if a != b => Effect::Widens,
          _ => continue,
        };
        let show = |v: Option<f64>| v.map_or_else(|| "none".to_owned(), |v| v.to_string());
        self.push(
          path,
          effect,
          format!("`{key}` changed from {} to {}", show(a), show(b)),
        );
      }
    }
    for key in ["pattern", "format", "uniqueItems"] {
      let (a, b) = (o.get(key), n.get(key));
      let effect = match (a, b) {
        (None, Some(Value::Bool(false))) | (Some(Value::Bool(false)), None) => continue,
        (None, Some(_)) => Effect::Narrows,
        (Some(_), None) => Effect::Widens,
        (Some(a), Some(b)) if a != b => Effect::Changes,
        _ => continue,
      };
      let show = |v: Option<&Value>| v.map_or_else(|| "none".to_owned(), Value::to_string);
      self.push(
        path,
        effect,
        format!("`{key}` changed from {} to {}", show(a), show(b)),
      );
    }
  }

  fn object(&mut self, o: &'a Map, n: &'a Map, path: &str) {
    let props = |m: &'a Map| -> BTreeMap<&'a str, &'a Value> {
      m.get("properties")
        .and_then(Value::as_object)
        .map(|p| p.iter().map(|(k, v)| (k.as_str(), v)).collect())
        .unwrap_or_default()
    };
    let required = |m: &'a Map| -> BTreeSet<&'a str> {
      m.get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
    };
    let (op, np, or, nr) = (props(o), props(n), required(o), required(n));
    let closed = matches!(n.get("additionalProperties"), Some(Value::Bool(false)));
    let input = self.dir == Dir::Input;
    for (name, ov) in &op {
      let at = format!("{path}.{name}");
      let Some(nv) = np.get(name) else {
        match (input, closed) {
          (true, true) => self.push(
            &at,
            Effect::Narrows,
            "field removed (unknown fields are refused)".to_owned(),
          ),
          (true, false) => self.push(
            &at,
            Effect::Neutral,
            "field removed (ignored when sent)".to_owned(),
          ),
          (false, _) => self.push(&at, Effect::Widens, "field removed".to_owned()),
        }
        continue;
      };
      match (or.contains(name), nr.contains(name)) {
        (true, false) => self.push(&at, Effect::Widens, "field became optional".to_owned()),
        (false, true) => self.push(&at, Effect::Narrows, "field became required".to_owned()),
        _ => {}
      }
      self.schema(ov, nv, &at);
    }
    for name in np.keys().filter(|k| !op.contains_key(*k)) {
      let at = format!("{path}.{name}");
      match (input, nr.contains(name)) {
        (true, true) => self.push(&at, Effect::Narrows, "required field added".to_owned()),
        (true, false) => self.push(&at, Effect::Widens, "optional field added".to_owned()),
        (false, true) => self.push(&at, Effect::Neutral, "field added".to_owned()),
        (false, false) => self.push(&at, Effect::Neutral, "optional field added".to_owned()),
      }
    }
    let at = format!("{path}.*");
    match (o.get("additionalProperties"), n.get("additionalProperties")) {
      (None | Some(Value::Bool(true)), Some(Value::Bool(false))) => {
        self.push(
          path,
          Effect::Narrows,
          "unknown fields now refused".to_owned(),
        );
      }
      (Some(Value::Bool(false)), None | Some(Value::Bool(true))) => {
        self.push(
          path,
          Effect::Widens,
          "unknown fields now allowed".to_owned(),
        );
      }
      (Some(a), Some(b)) if a.is_object() || b.is_object() => self.schema(a, b, &at),
      (Some(a), None) if a.is_object() => self.schema(a, &ANY, &at),
      (None, Some(b)) if b.is_object() => self.schema(&ANY, b, &at),
      _ => {}
    }
  }

  fn array(&mut self, o: &'a Map, n: &'a Map, path: &str) {
    match (o.get("items"), n.get("items")) {
      (None, None) => {}
      (a, b) => self.schema(a.unwrap_or(&ANY), b.unwrap_or(&ANY), &format!("{path}[]")),
    }
    let tuple = |m: &'a Map| m.get("prefixItems").and_then(Value::as_array);
    match (tuple(o), tuple(n)) {
      (Some(a), Some(b)) if a.len() == b.len() => {
        for (i, (a, b)) in a.iter().zip(b).enumerate() {
          self.schema(a, b, &format!("{path}[{i}]"));
        }
      }
      (None, None) => {}
      (a, b) => {
        let len =
          |t: Option<&Vec<Value>>| t.map_or_else(|| "none".to_owned(), |t| t.len().to_string());
        self.push(
          path,
          Effect::Changes,
          format!("tuple length changed from {} to {}", len(a), len(b)),
        );
      }
    }
  }

  /// Every keyword not compared above: equal normalized text, or a change both ways.
  fn rest(&mut self, o: &Map, n: &Map, path: &str) {
    let keys: BTreeSet<&String> = o.keys().chain(n.keys()).collect();
    for key in keys {
      if IGNORED.contains(&key.as_str()) || HANDLED.contains(&key.as_str()) {
        continue;
      }
      let a = o.get(key).map(|v| self.text(OLD, v));
      let b = n.get(key).map(|v| self.text(NEW, v));
      if a != b {
        self.push(path, Effect::Changes, format!("keyword `{key}` changed"));
      }
    }
  }

  /// Compare two error unions (the `error` of a procedure).
  fn errors(mut self, out: &mut Vec<Finding>) {
    let [old, new] = self.roots;
    match (self.variants(OLD, old), self.variants(NEW, new)) {
      (Some(o), Some(n)) => {
        for (tag, ov) in &o {
          match n.get(tag) {
            Some(nv) => self.schema(ov, nv, &format!("error({tag})")),
            None => self.found.push((
              "error".to_owned(),
              Class::Compatible,
              format!("error variant `{tag}` removed"),
            )),
          }
        }
        for tag in n.keys().filter(|t| !o.contains_key(*t)) {
          self.found.push((
            "error".to_owned(),
            Class::Breaking,
            format!("error variant `{tag}` added (old clients cannot decode it)"),
          ));
        }
      }
      _ => self.schema(old, new, "error"),
    }
    out.append(&mut self.found);
  }

  /// The variants of an error union by tag; `null` is the empty union, `None` another shape.
  fn variants(&self, side: usize, root: &'a Value) -> Option<BTreeMap<String, &'a Value>> {
    if root.is_null() {
      return Some(BTreeMap::new());
    }
    let (union, _) = self.deref(side, root);
    let arms = union
      .get("oneOf")
      .or_else(|| union.get("anyOf"))
      .and_then(Value::as_array)?;
    arms
      .iter()
      .map(|arm| tag_of(self.deref(side, arm).0).map(|tag| (tag, arm)))
      .collect()
  }
}

fn map_of(schema: &Value) -> &Map {
  static EMPTY: std::sync::OnceLock<Map> = std::sync::OnceLock::new();
  schema
    .as_object()
    .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

fn is_union(m: &Map) -> bool {
  m.contains_key("anyOf") || m.contains_key("oneOf")
}

/// A schema's alternatives: its `anyOf` / `oneOf`, or the schema itself.
fn arms(schema: &Value) -> Vec<&Value> {
  match schema
    .get("anyOf")
    .or_else(|| schema.get("oneOf"))
    .and_then(Value::as_array)
  {
    Some(arms) => arms.iter().collect(),
    None => vec![schema],
  }
}

/// The `_tag` const of a tagged-union alternative.
fn tag_of(schema: &Value) -> Option<String> {
  schema
    .get("properties")?
    .get("_tag")?
    .get("const")?
    .as_str()
    .map(str::to_owned)
}

fn types_text(types: &BTreeSet<String>) -> String {
  types
    .iter()
    .map(|t| format!("`{t}`"))
    .collect::<Vec<_>>()
    .join(" | ")
}

fn values_text(values: &[Value]) -> String {
  values
    .iter()
    .map(|v| format!("`{v}`"))
    .collect::<Vec<_>>()
    .join(", ")
}

/// A JSON Pointer token (`~1` is `/`, `~0` is `~`).
fn unescape(token: &str) -> String {
  token.replace("~1", "/").replace("~0", "~")
}

/// OpenAPI component names of `root`'s `$defs` keys: the last `::` segment, or the shortest
/// `::` suffix no other key shares, joined with `.`; the key itself when no suffix is unique.
fn def_names(root: &Value) -> BTreeMap<String, String> {
  let keys: Vec<&String> = root
    .get("$defs")
    .and_then(Value::as_object)
    .map(|d| d.keys().collect())
    .unwrap_or_default();
  let segments: Vec<Vec<&str>> = keys
    .iter()
    .map(|k| k.split("::").filter(|s| !s.is_empty()).collect())
    .collect();
  let suffix = |segs: &[&str], n: usize| segs[segs.len().saturating_sub(n)..].join(".");
  let mut out = BTreeMap::new();
  for (i, segs) in segments.iter().enumerate() {
    let unique = |n: usize| {
      let candidate = suffix(segs, n);
      let shared = segments
        .iter()
        .enumerate()
        .any(|(j, other)| j != i && other.len() >= n && suffix(other, n) == candidate);
      (!shared).then_some(candidate)
    };
    let name = (1..=segs.len())
      .find_map(unique)
      .unwrap_or_else(|| keys[i].clone());
    out.insert(keys[i].clone(), name);
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn diff(dir: Dir, old: Value, new: Value) -> Vec<Finding> {
    let mut out = Vec::new();
    Cmp::new(&old, &new, dir).run("input", &mut out);
    out
  }

  fn classes(found: &[Finding]) -> Vec<Class> {
    found.iter().map(|f| f.1).collect()
  }

  fn texts(found: &[Finding]) -> Vec<&str> {
    found.iter().map(|f| f.2.as_str()).collect()
  }

  #[test]
  fn def_names_follow_the_openapi_rule() {
    let root = json!({"$defs": {"a::Item": {}, "b::Item": {}, "x::y::Price": {}, "Solo": {}}});
    let names = def_names(&root);
    assert_eq!(names["a::Item"], "a.Item");
    assert_eq!(names["b::Item"], "b.Item");
    assert_eq!(names["x::y::Price"], "Price");
    assert_eq!(names["Solo"], "Solo");
    let same = json!({"$defs": {"a::Item": {}, "a::Item::": {}}});
    assert_eq!(def_names(&same)["a::Item"], "a::Item");
  }

  #[test]
  fn a_moved_module_is_no_change_and_a_rename_is_compatible() {
    let old =
      json!({"$ref": "#/$defs/app::v1::Price", "$defs": {"app::v1::Price": {"type": "integer"}}});
    let moved = json!({"$ref": "#/$defs/app::money::Price",
      "$defs": {"app::money::Price": {"type": "integer"}}});
    assert!(diff(Dir::Output, old.clone(), moved).is_empty());
    let renamed = json!({"$ref": "#/$defs/app::Cost", "$defs": {"app::Cost": {"type": "integer"}}});
    let found = diff(Dir::Output, old.clone(), renamed);
    assert_eq!(classes(&found), [Class::Compatible]);
    assert!(found[0].2.contains("`Price` renamed to `Cost`"));
    assert!(diff(Dir::Input, old, json!({"type": "integer"})).is_empty());
  }

  #[test]
  fn recursive_definitions_terminate() {
    let tree = |key: &str| {
      json!({"$ref": format!("#/$defs/{key}"), "$defs": {key: {"type": "object",
        "properties": {"kids": {"type": "array", "items": {"$ref": format!("#/$defs/{key}")}}}}}})
    };
    assert!(diff(Dir::Output, tree("a::Node"), tree("b::Node")).is_empty());
    let escaped = json!({"$ref": "#/$defs/a~1b~0c", "$defs": {"a/b~c": {"type": "string"}}});
    let found = diff(Dir::Input, escaped, json!({"type": "integer"}));
    assert_eq!(classes(&found), [Class::Breaking, Class::Compatible]);
  }

  #[test]
  fn types_narrow_and_widen_by_direction() {
    let s = |t: Value| json!({"type": t});
    let found = diff(Dir::Input, s(json!("number")), s(json!("integer")));
    assert_eq!(texts(&found), ["type `number` narrowed to `integer`"]);
    assert_eq!(classes(&found), [Class::Breaking]);
    let found = diff(Dir::Output, s(json!("integer")), s(json!("number")));
    assert_eq!(texts(&found), ["type `integer` widened to `number`"]);
    assert_eq!(classes(&found), [Class::Breaking]);
    let found = diff(
      Dir::Input,
      s(json!(["string", "null"])),
      s(json!(["string", "boolean"])),
    );
    assert_eq!(classes(&found), [Class::Breaking, Class::Compatible]);
    let found = diff(Dir::Input, json!({}), s(json!("string")));
    assert_eq!(classes(&found), [Class::Breaking]);
    let found = diff(Dir::Input, s(json!("string")), json!(true));
    assert_eq!(classes(&found), [Class::Compatible]);
    assert!(diff(Dir::Input, s(json!(7)), s(json!(7))).is_empty());
  }

  #[test]
  fn boolean_schemas() {
    assert!(diff(Dir::Input, json!(false), json!(false)).is_empty());
    assert_eq!(
      classes(&diff(Dir::Output, json!(false), json!({}))),
      [Class::Breaking]
    );
    assert_eq!(
      classes(&diff(Dir::Input, json!({}), json!(false))),
      [Class::Breaking]
    );
    assert_eq!(
      classes(&diff(Dir::Output, json!({}), json!(false))),
      [Class::Compatible]
    );
  }

  #[test]
  fn literal_values() {
    let found = diff(
      Dir::Output,
      json!({"enum": ["a", "b"]}),
      json!({"enum": ["a", "c"]}),
    );
    assert_eq!(classes(&found), [Class::Compatible, Class::Breaking]);
    let found = diff(Dir::Input, json!({}), json!({"const": 1}));
    assert_eq!(texts(&found), ["restricted to `1`"]);
    let found = diff(Dir::Input, json!({"const": 1}), json!({}));
    assert_eq!(classes(&found), [Class::Compatible]);
  }

  #[test]
  fn bounds_and_formats() {
    let found = diff(
      Dir::Input,
      json!({"minLength": 1, "maximum": 9}),
      json!({"minLength": 2, "maximum": 10}),
    );
    assert_eq!(classes(&found), [Class::Breaking, Class::Compatible]);
    let found = diff(
      Dir::Input,
      json!({"minLength": 3, "maxLength": 9}),
      json!({"minLength": 2, "maxLength": 8}),
    );
    assert_eq!(classes(&found), [Class::Compatible, Class::Breaking]);
    let found = diff(Dir::Input, json!({"minimum": 1}), json!({"maxItems": 3}));
    assert_eq!(classes(&found), [Class::Compatible, Class::Breaking]);
    assert!(found[0].2.contains("from 1 to none"));
    let found = diff(
      Dir::Input,
      json!({"pattern": "a"}),
      json!({"pattern": "b", "format": "date"}),
    );
    assert_eq!(classes(&found), [Class::Breaking, Class::Breaking]);
    let found = diff(
      Dir::Output,
      json!({"format": "date", "uniqueItems": true}),
      json!({"uniqueItems": false}),
    );
    assert_eq!(classes(&found), [Class::Breaking, Class::Breaking]);
    assert!(diff(Dir::Input, json!({}), json!({"uniqueItems": false})).is_empty());
    assert!(diff(Dir::Input, json!({"uniqueItems": false}), json!({})).is_empty());
  }

  #[test]
  fn objects() {
    let o = json!({"type": "object", "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
      "required": ["a"]});
    let n = json!({"type": "object", "properties": {"a": {"type": "string"}, "b": {"type": "string"},
      "c": {"type": "string"}}, "required": ["b", "c"], "additionalProperties": false});
    let found = diff(Dir::Input, o.clone(), n.clone());
    assert_eq!(
      texts(&found),
      [
        "field became optional",
        "field became required",
        "required field added",
        "unknown fields now refused"
      ]
    );
    assert_eq!(
      classes(&found),
      [
        Class::Compatible,
        Class::Breaking,
        Class::Breaking,
        Class::Breaking
      ]
    );
    let found = diff(Dir::Output, o.clone(), n.clone());
    assert_eq!(
      classes(&found),
      [
        Class::Breaking,
        Class::Compatible,
        Class::Compatible,
        Class::Compatible
      ]
    );
    let found = diff(Dir::Input, n.clone(), o.clone());
    assert_eq!(
      texts(&found),
      [
        "field became required",
        "field became optional",
        "field removed (ignored when sent)",
        "unknown fields now allowed"
      ]
    );
    let closed = json!({"type": "object", "properties": {}, "additionalProperties": false});
    let found = diff(Dir::Input, n.clone(), closed);
    assert_eq!(found.len(), 3);
    assert!(
      found
        .iter()
        .all(|f| f.2 == "field removed (unknown fields are refused)")
    );
    let found = diff(Dir::Output, n, o.clone());
    assert!(
      found
        .iter()
        .any(|f| f.0 == "input.c" && f.1 == Class::Breaking && f.2 == "field removed")
    );
    let opt = json!({"type": "object", "properties": {"a": {"type": "string"}, "z": {}}, "required": ["a"]});
    let found = diff(Dir::Output, o, opt);
    assert_eq!(texts(&found).last(), Some(&"optional field added"));
  }

  #[test]
  fn maps() {
    let map = |v: Value| json!({"type": "object", "additionalProperties": v});
    let found = diff(
      Dir::Output,
      map(json!({"type": "integer"})),
      map(json!({"type": "number"})),
    );
    assert_eq!(found[0].0, "input.*");
    assert_eq!(classes(&found), [Class::Breaking]);
    let found = diff(
      Dir::Input,
      map(json!({"type": "integer"})),
      json!({"type": "object"}),
    );
    assert_eq!(classes(&found), [Class::Compatible]);
    let found = diff(
      Dir::Input,
      json!({"type": "object"}),
      map(json!({"type": "integer"})),
    );
    assert_eq!(classes(&found), [Class::Breaking]);
    assert!(diff(Dir::Input, map(json!(true)), json!({"type": "object"})).is_empty());
  }

  #[test]
  fn arrays_and_tuples() {
    let found = diff(
      Dir::Input,
      json!({"items": {"type": "string"}}),
      json!({"items": {"type": "integer"}}),
    );
    assert_eq!(found[0].0, "input[]");
    let found = diff(Dir::Input, json!({}), json!({"items": {"type": "string"}}));
    assert_eq!(classes(&found), [Class::Breaking]);
    let t = |v: Value| json!({ "prefixItems": v });
    let found = diff(
      Dir::Input,
      t(json!([{"type": "string"}])),
      t(json!([{"type": "integer"}])),
    );
    assert_eq!(found[0].0, "input[0]");
    let found = diff(Dir::Input, t(json!([{}])), t(json!([{}, {}])));
    assert_eq!(texts(&found), ["tuple length changed from 1 to 2"]);
    let found = diff(Dir::Input, json!({}), t(json!([{}])));
    assert_eq!(texts(&found), ["tuple length changed from none to 1"]);
  }

  #[test]
  fn other_keywords_change_both_ways() {
    let found = diff(
      Dir::Input,
      json!({"allOf": [{"type": "string"}]}),
      json!({"allOf": [{"type": "integer"}]}),
    );
    assert_eq!(texts(&found), ["keyword `allOf` changed"]);
    assert_eq!(classes(&found), [Class::Breaking]);
    let annotations = json!({"description": "x", "title": "t", "allOf": [{"description": "y"}]});
    assert!(diff(Dir::Input, annotations, json!({"allOf": [{}]})).is_empty());
  }

  #[test]
  fn unions() {
    let opt = |t: Value| json!({"anyOf": [t, {"type": "null"}]});
    let found = diff(
      Dir::Output,
      json!({"type": "string"}),
      opt(json!({"type": "string"})),
    );
    assert_eq!(texts(&found), ["alternative `{\"type\":\"null\"}` added"]);
    assert_eq!(classes(&found), [Class::Breaking]);
    let found = diff(
      Dir::Input,
      opt(json!({"type": "string"})),
      json!({"type": "string"}),
    );
    assert_eq!(classes(&found), [Class::Breaking]);
    // One unmatched alternative on each side: compared structurally.
    let found = diff(
      Dir::Input,
      opt(json!({"type": "string"})),
      opt(json!({"type": "integer"})),
    );
    assert_eq!(classes(&found), [Class::Breaking, Class::Compatible]);
    let tagged = |tags: &[&str]| {
      let arms: Vec<Value> = tags
        .iter()
        .map(
          |t| json!({"type": "object", "properties": {"_tag": {"const": t}}, "required": ["_tag"]}),
        )
        .collect();
      json!({ "oneOf": arms })
    };
    let found = diff(
      Dir::Output,
      tagged(&["A", "B", "C"]),
      tagged(&["A", "D", "E"]),
    );
    assert_eq!(
      texts(&found),
      [
        "alternative `B` removed",
        "alternative `C` removed",
        "alternative `D` added",
        "alternative `E` added"
      ]
    );
    let refs = json!({"anyOf": [{"$ref": "#/$defs/a::P"}, {"type": "string",
      "pattern": "^[a-z]+[0-9]*-and-a-long-tail-that-goes-past-sixty-characters$"}]});
    let found = diff(Dir::Output, json!({"type": "integer"}), refs);
    assert!(found.iter().any(|f| f.2 == "alternative `a::P` added"));
    assert!(found.iter().any(|f| f.2.ends_with("…` added")));
    let with_defs = json!({"anyOf": [{"$ref": "#/$defs/x::P"}, {"type": "null"}],
      "$defs": {"x::P": {"type": "object", "properties": {"_tag": {"const": "P"}}}}});
    let other = json!({"anyOf": [{"$ref": "#/$defs/y::P"}, {"type": "null"}],
      "$defs": {"y::P": {"type": "object", "properties": {"_tag": {"const": "P"}, "n": {"type": "integer"}}}}});
    let found = diff(Dir::Output, with_defs, other);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "input(P).n");
  }

  fn proc(kind: ProcedureKind) -> ProcedureContract {
    ProcedureContract::new("p", kind, ProcedureAccess::Public {}, json!({}), json!({}))
  }

  fn reasons(old: &ProcedureContract, new: &ProcedureContract) -> Vec<(Class, String)> {
    procedure(old, new)
      .into_iter()
      .map(|(_, c, r)| (c, r))
      .collect()
  }

  #[test]
  fn procedure_attributes() {
    let q = proc(ProcedureKind::Query);
    let mut m = proc(ProcedureKind::Mutation);
    assert_eq!(
      reasons(&q, &m),
      [(
        Class::Breaking,
        "kind changed from `query` to `mutation`".to_owned()
      )]
    );
    let mut m2 = m.clone();
    m.idempotent = true;
    m2.deprecated = true;
    let found = reasons(&m, &m2);
    assert_eq!(found[0].0, Class::Breaking);
    assert_eq!(found[1], (Class::Compatible, "deprecated".to_owned()));
    let found = reasons(&m2, &m);
    assert_eq!(found[0], (Class::Compatible, "now idempotent".to_owned()));
    assert_eq!(
      found[1],
      (Class::Compatible, "no longer deprecated".to_owned())
    );
    let mut cs = proc(ProcedureKind::ClientStream);
    let mut cs2 = cs.clone();
    cs.max_items = Some(100);
    cs2.max_bytes = Some(1024);
    let found = reasons(&cs, &cs2);
    assert_eq!(
      found[0],
      (
        Class::Compatible,
        "max_items raised from 100 to 10000".to_owned()
      )
    );
    assert_eq!(
      found[1],
      (
        Class::Breaking,
        "max_bytes lowered from 4194304 to 1024".to_owned()
      )
    );
  }

  #[test]
  fn access_decisions() {
    let guarded = |p: &[&str]| ProcedureAccess::Guarded {
      policies: p.iter().map(|s| (*s).to_owned()).collect(),
    };
    let with = |a: ProcedureAccess| {
      let mut p = proc(ProcedureKind::Query);
      p.access = a;
      p
    };
    let public = with(ProcedureAccess::Public {});
    let staff = with(guarded(&["staff"]));
    let admin = with(guarded(&["admin", "staff"]));
    assert_eq!(
      reasons(&public, &staff),
      [(
        Class::Breaking,
        "access tightened: public, now guarded by `staff`".to_owned()
      )]
    );
    assert_eq!(reasons(&staff, &public)[0].0, Class::Compatible);
    assert_eq!(reasons(&staff, &admin)[0].0, Class::Breaking);
    assert_eq!(reasons(&admin, &staff)[0].0, Class::Compatible);
    assert!(reasons(&public, &public).is_empty());
  }

  #[test]
  fn error_unions() {
    let variant = |tag: &str, fields: Value| {
      let mut props = json!({"_tag": {"const": tag}});
      if let (Some(p), Some(f)) = (props.as_object_mut(), fields.as_object()) {
        p.extend(f.clone());
      }
      json!({"type": "object", "properties": props, "required": ["_tag"]})
    };
    let with = |error: Option<Value>| {
      let mut p = proc(ProcedureKind::Query);
      p.error = error;
      p
    };
    let a = with(Some(json!({"oneOf": [variant("A", json!({}))]})));
    let ab = with(Some(json!({"oneOf": [
      variant("A", json!({"n": {"type": "integer"}})), variant("B", json!({}))]})));
    let found = procedure(&a, &ab);
    assert_eq!(
      found[0],
      (
        "error(A).n".to_owned(),
        Class::Compatible,
        "optional field added".to_owned()
      )
    );
    assert_eq!(found[1].1, Class::Breaking);
    assert!(found[1].2.starts_with("error variant `B` added"));
    let found = procedure(&ab, &with(None));
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|f| f.1 == Class::Compatible));
    assert_eq!(procedure(&with(None), &a)[0].1, Class::Breaking);
    let odd = with(Some(json!({"type": "string"})));
    let found = procedure(&odd, &with(Some(json!({"type": "integer"}))));
    assert_eq!(found[0].0, "error");
  }

  #[test]
  fn change_display_and_errors() {
    let c = Change {
      class: Class::Versioned,
      service: "S".to_owned(),
      version: 2,
      procedure: None,
      path: String::new(),
      reason: "r".to_owned(),
    };
    assert_eq!(c.to_string(), "versioned  S v2: r");
    assert_eq!(Class::Compatible.to_string(), "compatible");
    let io = CheckError::Read {
      path: "x".into(),
      source: std::io::Error::other("gone"),
    };
    assert!(std::error::Error::source(&io).is_some());
    let git = CheckError::Git {
      spec: "a:b".to_owned(),
      message: "m".to_owned(),
    };
    assert_eq!(git.to_string(), "cannot read `a:b` from git: m");
    assert!(std::error::Error::source(&git).is_none());
    let bad = CheckError::Contract {
      what: "w".to_owned(),
      source: ContractError::Json("j".to_owned()),
    };
    assert!(std::error::Error::source(&bad).is_some());
    assert!(bad.to_string().starts_with("w: "));
    let refused = read_against("-x:y", Path::new("."))
      .err()
      .map(|e| e.to_string());
    assert!(refused.is_some_and(|e| e.contains("cannot start with `-`")));
  }
}
