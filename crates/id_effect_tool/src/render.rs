//! Rendering a template into files in memory: [`Options`] in, [`RenderedFile`]s out.
//!
//! Template syntax (kept small on purpose):
//!
//! - `{{name}}`, `{{crate_name}}`, `{{id_effect_version}}` and `{{kit_features}}` are replaced;
//!   any other `{{word}}` is an error. `{{` followed by a space (`${{ github.ref }}` in a
//!   workflow) is left alone.
//! - A line holding only `{{#if flag}}` or `{{#if !flag}}` opens a block that a line holding only
//!   `{{/if}}` closes; blocks nest. Flags: `pg`, `otel`, `jobs`, `console` and the template
//!   names (`service`, `worker`, `cli`, `cluster`).

use std::path::{Path, PathBuf};

use crate::error::NewError;
use crate::template::{Condition, Template, TemplateFile, output_path};

/// The id_effect version generated projects depend on (this tool's own version).
pub const ID_EFFECT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The database the `service` template wires in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Db {
  /// No database: the service boots with no other services running.
  #[default]
  None,
  /// PostgreSQL through `id_effect_sql` (kit features `sql-pg` and `migrate`), with a first
  /// migration applied on start.
  Pg,
}

/// What `id-effect new` generates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
  /// The package (and binary) name, e.g. `orders-api`.
  pub name: String,
  /// The template.
  pub template: Template,
  /// The database (`service` only).
  pub db: Db,
  /// Export traces, metrics and logs over OTLP (`service` only).
  pub otel: bool,
  /// Background jobs: `src/jobs/` and a worker running in the service's process (`service`
  /// only).
  pub jobs: bool,
  /// The operator console on a loopback port, behind an authorization guard (`service` only).
  pub console: bool,
  /// An id_effect checkout to build against: adds a `[patch.crates-io]` section pointing every
  /// id_effect crate the project uses at `<path>/crates/<crate>`.
  pub local_workspace: Option<PathBuf>,
}

impl Options {
  /// `name` from `template`, with no database, no OpenTelemetry, no jobs, no console and
  /// crates.io dependencies.
  pub fn new(name: impl Into<String>, template: Template) -> Self {
    Self {
      name: name.into(),
      template,
      db: Db::None,
      otel: false,
      jobs: false,
      console: false,
      local_workspace: None,
    }
  }

  /// Sets the database.
  pub fn db(mut self, db: Db) -> Self {
    self.db = db;
    self
  }

  /// Turns OpenTelemetry export on or off.
  pub fn otel(mut self, on: bool) -> Self {
    self.otel = on;
    self
  }

  /// Turns the background jobs (and their in-process worker) on or off.
  pub fn jobs(mut self, on: bool) -> Self {
    self.jobs = on;
    self
  }

  /// Turns the operator console on or off.
  pub fn console(mut self, on: bool) -> Self {
    self.console = on;
    self
  }

  /// Builds against the id_effect checkout at `path` (see [`Options::local_workspace`]).
  pub fn local_workspace(mut self, path: impl Into<PathBuf>) -> Self {
    self.local_workspace = Some(path.into());
    self
  }

  /// The Rust crate name: the package name with `-` replaced by `_`.
  pub fn crate_name(&self) -> String {
    self.name.replace('-', "_")
  }

  /// Checks the name and that the template supports the options.
  ///
  /// # Errors
  ///
  /// [`NewError::InvalidName`] or [`NewError::Unsupported`].
  pub fn validate(&self) -> Result<(), NewError> {
    validate_name(&self.name)?;
    if self.template != Template::Service {
      let template = self.template.name();
      if self.db == Db::Pg {
        return Err(NewError::Unsupported {
          option: "--db pg",
          template,
        });
      }
      if self.otel {
        return Err(NewError::Unsupported {
          option: "--otel",
          template,
        });
      }
      if self.jobs {
        return Err(NewError::Unsupported {
          option: "--jobs",
          template,
        });
      }
      if self.console {
        return Err(NewError::Unsupported {
          option: "--console",
          template,
        });
      }
    }
    Ok(())
  }

  /// The `id_effect_kit` features the generated project enables.
  pub fn kit_features(&self) -> Vec<&'static str> {
    let mut features = Vec::new();
    match self.template {
      Template::Service => {
        // RPC next to the REST routes, HTTP/3 on by default (ADR 0014 D4).
        features.extend(["web", "rpc", "http3"]);
        if self.db == Db::Pg {
          features.extend(["sql-pg", "migrate"]);
        }
        if self.otel {
          features.push("otel");
        }
        if self.jobs {
          features.push("jobs");
        }
        if self.console {
          features.push("console");
        }
      }
      Template::Cluster => features.push("cluster"),
      Template::Worker | Template::Cli => {}
    }
    features
  }

  /// The id_effect crates in the generated project's dependency graph: the ones a
  /// `--local-workspace` patch points at a checkout (an unused patch is a cargo warning, a
  /// missing one silently mixes crates.io and local crates).
  pub fn workspace_crates(&self) -> Vec<&'static str> {
    let mut crates = vec![
      "id_effect",
      "id_effect_macro",
      "id_effect_proc_macro",
      "id_effect_cli",
      "id_effect_config",
      "id_effect_kit",
    ];
    for feature in self.kit_features() {
      crates.extend_from_slice(match feature {
        "web" => &["id_effect_axum", "id_effect_platform"][..],
        "sql-pg" | "migrate" => &["id_effect_sql"],
        "otel" => &["id_effect_opentelemetry", "id_effect_logger"],
        "cluster" => &["id_effect_node", "id_effect_platform", "id_effect_process"],
        "rpc" => &["id_effect_rpc", "id_effect_rpc_macros"],
        "jobs" => &["id_effect_jobs", "id_effect_sql"],
        "console" => &[
          "id_effect_console",
          "id_effect_authz",
          "id_effect_logger",
          "id_effect_node",
          "id_effect_process",
        ],
        _ => &[],
      });
    }
    crates.sort_unstable();
    crates.dedup();
    crates
  }

  fn flag(&self, flag: &str) -> Option<bool> {
    match flag {
      "pg" => Some(self.db == Db::Pg),
      "otel" => Some(self.otel),
      "jobs" => Some(self.jobs),
      "console" => Some(self.console),
      _ => flag.parse::<Template>().ok().map(|t| t == self.template),
    }
  }

  fn includes(&self, file: &TemplateFile) -> bool {
    match file.when {
      Condition::Always => true,
      Condition::Pg => self.db == Db::Pg,
      Condition::Jobs => self.jobs,
      Condition::Console => self.console,
    }
  }
}

/// Rust keywords (2024 edition, incl. reserved): a crate name must not be one.
const KEYWORDS: &[&str] = &[
  "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate", "do",
  "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let",
  "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return",
  "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof", "unsafe",
  "unsized", "use", "virtual", "where", "while", "yield",
];

/// Crates a generated project depends on (or that shadow the standard library): a package with
/// the same name could not depend on them.
const TAKEN: &[&str] = &[
  "alloc",
  "axum",
  "clap",
  "core",
  "postcard",
  "proc_macro",
  "serde",
  "std",
  "test",
  "tokio",
  "tower",
];

fn validate_name(name: &str) -> Result<(), NewError> {
  let invalid = |reason| {
    Err(NewError::InvalidName {
      name: name.to_owned(),
      reason,
    })
  };
  let Some(first) = name.chars().next() else {
    return invalid("it is empty");
  };
  if name.len() > 64 {
    return invalid("it is longer than 64 characters");
  }
  if !first.is_ascii_lowercase() {
    return invalid("it must start with a lowercase ASCII letter");
  }
  // Lowercase only: the name is also the Docker image and Kamal service name.
  if !name
    .chars()
    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
  {
    return invalid("use only lowercase ASCII letters, digits, `-` and `_`");
  }
  let crate_name = name.replace('-', "_");
  if KEYWORDS.contains(&crate_name.as_str()) {
    return invalid("it is a Rust keyword");
  }
  if TAKEN.contains(&crate_name.as_str()) || crate_name.starts_with("id_effect") {
    return invalid("it is the name of a crate the project depends on");
  }
  Ok(())
}

/// One generated file: its path relative to the project root and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedFile {
  /// Path relative to the project root, `/`-separated (e.g. `.github/workflows/ci.yml`).
  pub path: String,
  /// The rendered text.
  pub contents: String,
}

/// Renders `options.template` in memory (nothing touches the disk except reading the
/// `--local-workspace` directory).
///
/// # Errors
///
/// [`NewError::InvalidName`] / [`NewError::Unsupported`] (see [`Options::validate`]),
/// [`NewError::NotAWorkspace`] for a bad `--local-workspace`, [`NewError::Template`] for a
/// malformed template.
pub fn render(options: &Options) -> Result<Vec<RenderedFile>, NewError> {
  options.validate()?;
  let patch = match &options.local_workspace {
    Some(path) => Some(patch_section(path, &options.workspace_crates())?),
    None => None,
  };
  let features = options
    .kit_features()
    .iter()
    .map(|f| format!("\"{f}\""))
    .collect::<Vec<_>>()
    .join(", ");
  let crate_name = options.crate_name();
  let vars = [
    ("name", options.name.as_str()),
    ("crate_name", crate_name.as_str()),
    ("id_effect_version", ID_EFFECT_VERSION),
    ("kit_features", features.as_str()),
  ];
  let mut out = Vec::new();
  for file in options.template.files().filter(|f| options.includes(f)) {
    let label = format!("{}/{}", file.dir, file.source);
    let kept = keep_lines(file.contents, options, &label)?;
    let mut contents = substitute(&kept, &vars, &label)?;
    let path = output_path(file.source);
    if path == "Cargo.toml"
      && let Some(patch) = &patch
    {
      contents.push_str(patch);
    }
    out.push(RenderedFile { path, contents });
  }
  Ok(out)
}

/// `path` as Cargo reads it in a TOML string: forward slashes (valid on every platform, no
/// escaping) and no Windows verbatim prefix, which `canonicalize` adds (`\\?\D:\x`) and Cargo
/// rejects as a path URL.
pub(crate) fn toml_path(path: &Path) -> String {
  let text = path.to_string_lossy();
  let plain = match text.strip_prefix(r"\\?\UNC\") {
    Some(share) => format!(r"\\{share}"),
    None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned(),
  };
  plain.replace('\\', "/")
}

/// The `[patch.crates-io]` section for a `--local-workspace` checkout at `workspace`.
fn patch_section(workspace: &Path, crates: &[&str]) -> Result<String, NewError> {
  let not_a_workspace = || NewError::NotAWorkspace(workspace.to_path_buf());
  let root = workspace.canonicalize().map_err(|_| not_a_workspace())?;
  if !root.join("crates/id_effect_kit/Cargo.toml").is_file() {
    return Err(not_a_workspace());
  }
  let root = toml_path(&root);
  let mut section = format!(
    "\n# --local-workspace: build against the id_effect checkout at {root}.\n[patch.crates-io]\n"
  );
  for name in crates {
    section.push_str(&format!("{name} = {{ path = \"{root}/crates/{name}\" }}\n"));
  }
  Ok(section)
}

/// Applies the `{{#if}}` / `{{/if}}` line blocks.
fn keep_lines(text: &str, options: &Options, label: &str) -> Result<String, NewError> {
  keep_lines_with(text, &|flag| options.flag(flag), label)
}

/// [`keep_lines`] with the flags `lookup` knows (`None`: an unknown flag); `id-effect generate`
/// renders its templates with flags of its own.
pub(crate) fn keep_lines_with(
  text: &str,
  lookup: &dyn Fn(&str) -> Option<bool>,
  label: &str,
) -> Result<String, NewError> {
  let error = |reason: String| NewError::Template {
    file: label.to_owned(),
    reason,
  };
  let mut stack: Vec<bool> = Vec::new();
  let mut out = String::with_capacity(text.len());
  for (n, line) in text.split_inclusive('\n').enumerate() {
    let trimmed = line.trim();
    if let Some(cond) = trimmed
      .strip_prefix("{{#if ")
      .and_then(|c| c.strip_suffix("}}"))
    {
      let (negate, flag) = match cond.trim().strip_prefix('!') {
        Some(flag) => (true, flag),
        None => (false, cond.trim()),
      };
      let value =
        lookup(flag).ok_or_else(|| error(format!("line {}: unknown flag `{flag}`", n + 1)))?;
      stack.push(value != negate);
    } else if trimmed == "{{/if}}" {
      stack.pop().ok_or_else(|| {
        error(format!(
          "line {}: `{{{{/if}}}}` without `{{{{#if}}}}`",
          n + 1
        ))
      })?;
    } else if stack.iter().all(|&keep| keep) {
      out.push_str(line);
    }
  }
  if stack.is_empty() {
    Ok(out)
  } else {
    Err(error(format!("{} unclosed `{{{{#if}}}}`", stack.len())))
  }
}

/// Replaces the `{{word}}` placeholders.
pub(crate) fn substitute(
  text: &str,
  vars: &[(&str, &str)],
  label: &str,
) -> Result<String, NewError> {
  let mut out = String::with_capacity(text.len());
  let mut rest = text;
  while let Some(start) = rest.find("{{") {
    out.push_str(&rest[..start]);
    let after = &rest[start + 2..];
    let word_len = after
      .find(|c: char| !(c.is_ascii_lowercase() || c == '_' || c == '#' || c == '/' || c == '!'))
      .unwrap_or(after.len());
    if word_len == 0 || !after[word_len..].starts_with("}}") {
      // Not a placeholder (`${{ github.ref }}`): keep the braces verbatim.
      out.push_str("{{");
      rest = after;
      continue;
    }
    let word = &after[..word_len];
    let value = vars
      .iter()
      .find(|(key, _)| *key == word)
      .map(|(_, value)| *value)
      .ok_or_else(|| NewError::Template {
        file: label.to_owned(),
        reason: format!("unknown placeholder `{{{{{word}}}}}`"),
      })?;
    out.push_str(value);
    rest = &after[word_len + 2..];
  }
  out.push_str(rest);
  Ok(out)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn file<'a>(files: &'a [RenderedFile], path: &str) -> &'a str {
    &files
      .iter()
      .find(|f| f.path == path)
      .unwrap_or_else(|| panic!("no {path}"))
      .contents
  }

  fn paths(files: &[RenderedFile]) -> Vec<&str> {
    files.iter().map(|f| f.path.as_str()).collect()
  }

  mod options {
    use super::*;

    #[test]
    fn defaults_are_no_db_no_otel_crates_io() {
      let o = Options::new("app", Template::Service);
      assert_eq!((o.db, o.otel, o.local_workspace), (Db::None, false, None));
    }

    #[test]
    fn crate_name_replaces_dashes() {
      assert_eq!(
        Options::new("orders-api", Template::Cli).crate_name(),
        "orders_api"
      );
    }

    #[test]
    fn service_features_follow_the_options() {
      let base = Options::new("app", Template::Service);
      assert_eq!(base.kit_features(), ["web", "rpc", "http3"]);
      let full = base.db(Db::Pg).otel(true);
      assert_eq!(
        full.kit_features(),
        ["web", "rpc", "http3", "sql-pg", "migrate", "otel"]
      );
      let ops = Options::new("app", Template::Service)
        .jobs(true)
        .console(true);
      assert_eq!(
        ops.kit_features(),
        ["web", "rpc", "http3", "jobs", "console"]
      );
    }

    #[test]
    fn jobs_and_console_patch_the_crates_they_bring() {
      let svc = Options::new("a", Template::Service)
        .jobs(true)
        .console(true)
        .workspace_crates();
      for c in [
        "id_effect_jobs",
        "id_effect_sql",
        "id_effect_console",
        "id_effect_authz",
      ] {
        assert!(svc.contains(&c), "{c}");
      }
      let plain = Options::new("a", Template::Service).workspace_crates();
      assert!(!plain.contains(&"id_effect_jobs") && !plain.contains(&"id_effect_console"));
    }

    #[test]
    fn other_templates_have_fixed_features() {
      assert_eq!(
        Options::new("a", Template::Cluster).kit_features(),
        ["cluster"]
      );
      assert!(
        Options::new("a", Template::Worker)
          .kit_features()
          .is_empty()
      );
      assert!(Options::new("a", Template::Cli).kit_features().is_empty());
    }

    #[test]
    fn workspace_crates_cover_the_enabled_batteries() {
      let cli = Options::new("a", Template::Cli).workspace_crates();
      assert!(cli.contains(&"id_effect_kit") && !cli.contains(&"id_effect_axum"));
      let svc = Options::new("a", Template::Service)
        .db(Db::Pg)
        .otel(true)
        .workspace_crates();
      for c in ["id_effect_axum", "id_effect_sql", "id_effect_opentelemetry"] {
        assert!(svc.contains(&c), "{c}");
      }
      let node = Options::new("a", Template::Cluster).workspace_crates();
      assert!(node.contains(&"id_effect_node") && node.contains(&"id_effect_process"));
    }

    #[test]
    fn db_and_otel_are_service_only() {
      for template in [Template::Worker, Template::Cli, Template::Cluster] {
        let pg = Options::new("a", template)
          .db(Db::Pg)
          .validate()
          .unwrap_err();
        assert!(matches!(
          pg,
          NewError::Unsupported {
            option: "--db pg",
            ..
          }
        ));
        let otel = Options::new("a", template)
          .otel(true)
          .validate()
          .unwrap_err();
        assert!(matches!(
          otel,
          NewError::Unsupported {
            option: "--otel",
            ..
          }
        ));
        let jobs = Options::new("a", template)
          .jobs(true)
          .validate()
          .unwrap_err();
        assert!(matches!(
          jobs,
          NewError::Unsupported {
            option: "--jobs",
            ..
          }
        ));
        let console = Options::new("a", template)
          .console(true)
          .validate()
          .unwrap_err();
        assert!(matches!(
          console,
          NewError::Unsupported {
            option: "--console",
            ..
          }
        ));
      }
      assert!(
        Options::new("a", Template::Service)
          .db(Db::Pg)
          .otel(true)
          .validate()
          .is_ok()
      );
    }
  }

  mod validate_name {
    use super::*;

    #[test]
    fn accepts_package_names() {
      for name in ["app", "orders-api", "my_worker2", "x"] {
        assert!(validate_name(name).is_ok(), "{name}");
      }
    }

    #[test]
    fn rejects_bad_names() {
      let long = "a".repeat(65);
      for name in [
        "",
        "9lives",
        "-x",
        "Upper",
        "lowerUpper",
        "has space",
        "dot.ted",
        "fn",
        "self",
        "tokio",
        "id-effect-app",
        long.as_str(),
      ] {
        assert!(
          matches!(validate_name(name), Err(NewError::InvalidName { .. })),
          "{name}"
        );
      }
    }
  }

  mod render {
    use super::*;

    #[test]
    fn every_template_renders_with_no_placeholder_left() {
      for template in Template::ALL {
        let files = render(&Options::new("demo-app", template)).expect("render");
        for f in &files {
          for leftover in [
            "{{name",
            "{{crate_name",
            "{{#if",
            "{{/if",
            "{{id_effect",
            "{{kit",
          ] {
            assert!(
              !f.contents.contains(leftover),
              "{template} {}: {leftover}",
              f.path
            );
          }
        }
      }
    }

    #[test]
    fn every_service_variant_renders() {
      for db in [Db::None, Db::Pg] {
        for otel in [false, true] {
          let o = Options::new("svc", Template::Service).db(db).otel(otel);
          let files = render(&o).expect("render");
          let cargo = file(&files, "Cargo.toml");
          assert_eq!(cargo.contains("\"sql-pg\""), db == Db::Pg, "{cargo}");
          assert_eq!(cargo.contains("\"migrate\""), db == Db::Pg, "{cargo}");
          assert_eq!(cargo.contains("\"otel\""), otel, "{cargo}");
          assert_eq!(
            paths(&files).contains(&"migrations/0001_init.sql"),
            db == Db::Pg
          );
          if db == Db::Pg {
            let db_rs = file(&files, "src/db.rs");
            assert!(db_rs.contains("migrate::run"), "{db_rs}");
            assert!(!db_rs.contains("include_str!"), "{db_rs}");
          }
          let ci = file(&files, ".github/workflows/ci.yml");
          assert_eq!(ci.contains("id-effect migrate check"), db == Db::Pg, "{ci}");
          let main = file(&files, "src/main.rs");
          assert_eq!(main.contains("install_from_config"), otel);
        }
      }
    }

    #[test]
    fn substitutes_name_crate_name_and_version() {
      let files = render(&Options::new("orders-api", Template::Cli)).expect("render");
      let cargo = file(&files, "Cargo.toml");
      assert!(cargo.contains("name = \"orders-api\""), "{cargo}");
      assert!(
        cargo.contains(&format!("version = \"{ID_EFFECT_VERSION}\"")),
        "{cargo}"
      );
      assert!(file(&files, "src/main.rs").contains("orders_api::"));
    }

    #[test]
    fn cargo_toml_depends_on_the_kit_and_the_core_crate() {
      for template in Template::ALL {
        let files = render(&Options::new("demo", template)).expect("render");
        let cargo = file(&files, "Cargo.toml");
        assert!(cargo.contains("\nid_effect_kit = {"), "{template}: {cargo}");
        assert!(cargo.contains("\nid_effect = "), "{template}: {cargo}");
        assert!(!cargo.contains("[patch.crates-io]"), "{template}");
      }
    }

    #[test]
    fn writes_dot_paths_and_keeps_workflow_expressions() {
      let files = render(&Options::new("demo", Template::Worker)).expect("render");
      let all = paths(&files);
      for p in [
        ".github/workflows/ci.yml",
        ".env.example",
        ".gitignore",
        ".dockerignore",
      ] {
        assert!(all.contains(&p), "{p}: {all:?}");
      }
      assert!(file(&files, ".github/workflows/ci.yml").contains("${{ "));
    }

    #[test]
    fn toml_paths_drop_the_windows_verbatim_prefix() {
      use crate::render::toml_path;
      assert_eq!(
        toml_path(Path::new(r"\\?\D:\a\id_effect")),
        "D:/a/id_effect"
      );
      assert_eq!(
        toml_path(Path::new(r"\\?\UNC\srv\share\x")),
        "//srv/share/x"
      );
      assert_eq!(
        toml_path(Path::new("/home/me/id_effect")),
        "/home/me/id_effect"
      );
    }

    #[test]
    fn local_workspace_patches_every_used_crate() {
      let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
      let o = Options::new("svc", Template::Service).local_workspace(&root);
      let files = render(&o).expect("render");
      let cargo = file(&files, "Cargo.toml");
      let root = crate::render::toml_path(&root.canonicalize().unwrap());
      assert!(cargo.contains("[patch.crates-io]"), "{cargo}");
      for name in o.workspace_crates() {
        let line = format!("{name} = {{ path = \"{root}/crates/{name}\" }}");
        assert!(cargo.contains(&line), "{line}\n{cargo}");
      }
    }

    #[test]
    fn local_workspace_must_be_a_checkout() {
      let dir = tempfile::tempdir().unwrap();
      for path in [dir.path().to_path_buf(), dir.path().join("missing")] {
        let o = Options::new("svc", Template::Service).local_workspace(&path);
        assert!(matches!(render(&o), Err(NewError::NotAWorkspace(p)) if p == path));
      }
    }

    #[test]
    fn invalid_options_render_nothing() {
      assert!(matches!(
        render(&Options::new("9", Template::Cli)),
        Err(NewError::InvalidName { .. })
      ));
    }
  }

  mod keep_lines {
    use super::*;

    fn keep(text: &str, options: &Options) -> Result<String, NewError> {
      keep_lines(text, options, "t")
    }

    #[test]
    fn keeps_blocks_whose_flag_holds_and_drops_the_markers() {
      let o = Options::new("a", Template::Service).db(Db::Pg);
      let text =
        "a\n{{#if pg}}\nb\n{{/if}}\n{{#if !pg}}\nc\n{{/if}}\n  {{#if otel}}\nd\n  {{/if}}\ne\n";
      assert_eq!(keep(text, &o).unwrap(), "a\nb\ne\n");
    }

    #[test]
    fn nests_and_knows_template_flags() {
      let o = Options::new("a", Template::Worker);
      let text = "{{#if worker}}\nw\n{{#if service}}\ns\n{{/if}}\n{{/if}}\n";
      assert_eq!(keep(text, &o).unwrap(), "w\n");
    }

    #[test]
    fn rejects_unknown_flags_and_unbalanced_blocks() {
      let o = Options::new("a", Template::Cli);
      for (text, want) in [
        ("{{#if nope}}\n{{/if}}\n", "unknown flag `nope`"),
        ("{{/if}}\n", "without"),
        ("{{#if pg}}\nx\n", "1 unclosed"),
      ] {
        match keep(text, &o) {
          Err(NewError::Template { file, reason }) => {
            assert_eq!(file, "t");
            assert!(reason.contains(want), "{reason}");
          }
          other => panic!("{text:?}: {other:?}"),
        }
      }
    }
  }

  mod substitute {
    use super::*;

    const VARS: &[(&str, &str)] = &[("name", "app"), ("crate_name", "app_x")];

    #[test]
    fn replaces_known_placeholders() {
      let out = substitute("{{name}} uses {{crate_name}}::run", VARS, "t").unwrap();
      assert_eq!(out, "app uses app_x::run");
    }

    #[test]
    fn leaves_non_placeholders_verbatim() {
      for text in ["${{ github.ref }}", "{{", "a {{Name}} b", "{{name", "x}}"] {
        assert_eq!(substitute(text, VARS, "t").unwrap(), text);
      }
    }

    #[test]
    fn rejects_unknown_placeholders() {
      let err = substitute("{{nope}}", VARS, "t").unwrap_err();
      assert!(
        err.to_string().contains("unknown placeholder `{{nope}}`"),
        "{err}"
      );
    }
  }
}
