//! The project templates, embedded in the binary.
//!
//! Every file under `templates/<dir>/` is listed once in [`FILES`] (a test checks the table
//! against the directory). On disk each file ends in `.template` and a leading `_` stands for a
//! leading `.` (`_github/` → `.github/`, `_env.example` → `.env.example`), so neither formatters
//! nor `cargo package` (which drops subdirectories that hold a `Cargo.toml`) touch them;
//! [`output_path`] maps a source path to the generated one.

use std::fmt;
use std::str::FromStr;

/// A project template `id-effect new` renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Template {
  /// An Axum HTTP service: secure defaults, `/health`, `/ready`, `/metrics`; optional
  /// PostgreSQL and OpenTelemetry.
  Service,
  /// A background worker: named cron jobs with run history.
  Worker,
  /// A command-line program: `run_main` and shell completions.
  Cli,
  /// A cluster node: an `id_effect_node` mesh hosting a process behavior.
  Cluster,
}

impl Template {
  /// Every template, in `--help` order.
  pub const ALL: [Self; 4] = [Self::Service, Self::Worker, Self::Cli, Self::Cluster];

  /// The template's name (`service`, `worker`, `cli`, `cluster`): its `--template` value and
  /// its directory under `templates/`.
  pub fn name(self) -> &'static str {
    match self {
      Self::Service => "service",
      Self::Worker => "worker",
      Self::Cli => "cli",
      Self::Cluster => "cluster",
    }
  }

  /// The embedded files of this template: the shared `common` files, then its own.
  pub fn files(self) -> impl Iterator<Item = &'static TemplateFile> {
    FILES
      .iter()
      .filter(move |f| f.dir == COMMON || f.dir == self.name())
  }
}

impl fmt::Display for Template {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.name())
  }
}

impl FromStr for Template {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::ALL
      .into_iter()
      .find(|t| t.name() == s)
      .ok_or_else(|| format!("unknown template `{s}` (expected service, worker, cli or cluster)"))
  }
}

/// When an embedded file is part of the generated project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
  /// Always.
  Always,
  /// Only with `--db pg`.
  Pg,
}

/// One embedded template file.
#[derive(Clone, Copy, Debug)]
pub struct TemplateFile {
  /// Directory under `templates/`: a template name or `common` (shared by every template).
  pub dir: &'static str,
  /// Path inside `dir`, as on disk (with the `.template` suffix and `_` prefixes).
  pub source: &'static str,
  /// The file's text.
  pub contents: &'static str,
  /// When the file is generated.
  pub when: Condition,
}

/// The directory of the files every template shares.
pub const COMMON: &str = "common";

macro_rules! embed {
  ($dir:literal, $path:literal) => {
    embed!($dir, $path, Condition::Always)
  };
  ($dir:literal, $path:literal, $when:expr) => {
    TemplateFile {
      dir: $dir,
      source: $path,
      contents: include_str!(concat!("../templates/", $dir, "/", $path)),
      when: $when,
    }
  };
}

/// Every embedded template file (`templates/<dir>/<source>`).
pub static FILES: &[TemplateFile] = &[
  embed!("common", "Dockerfile.template"),
  embed!("common", "_dockerignore.template"),
  embed!("common", "_gitignore.template"),
  embed!("common", "_github/workflows/ci.yml.template"),
  embed!("service", "Cargo.toml.template"),
  embed!("service", "README.md.template"),
  embed!("service", "compose.yaml.template"),
  embed!("service", "_env.example.template"),
  embed!("service", "config/deploy.yml.template"),
  embed!("service", "src/lib.rs.template"),
  embed!("service", "src/main.rs.template"),
  embed!("service", "src/db.rs.template", Condition::Pg),
  embed!(
    "service",
    "migrations/0001_init.sql.template",
    Condition::Pg
  ),
  embed!("service", "tests/http.rs.template"),
  embed!("worker", "Cargo.toml.template"),
  embed!("worker", "README.md.template"),
  embed!("worker", "compose.yaml.template"),
  embed!("worker", "_env.example.template"),
  embed!("worker", "src/lib.rs.template"),
  embed!("worker", "src/main.rs.template"),
  embed!("worker", "tests/jobs.rs.template"),
  embed!("cli", "Cargo.toml.template"),
  embed!("cli", "README.md.template"),
  embed!("cli", "compose.yaml.template"),
  embed!("cli", "_env.example.template"),
  embed!("cli", "src/lib.rs.template"),
  embed!("cli", "src/main.rs.template"),
  embed!("cli", "tests/cli.rs.template"),
  embed!("cluster", "Cargo.toml.template"),
  embed!("cluster", "README.md.template"),
  embed!("cluster", "compose.yaml.template"),
  embed!("cluster", "_env.example.template"),
  embed!("cluster", "src/lib.rs.template"),
  embed!("cluster", "src/main.rs.template"),
  embed!("cluster", "tests/mesh.rs.template"),
];

/// The generated path of the template file `source`: the `.template` suffix is dropped and a
/// leading `_` of each path component becomes `.`.
///
/// ```rust
/// use id_effect_tool::template::output_path;
///
/// assert_eq!(output_path("Cargo.toml.template"), "Cargo.toml");
/// assert_eq!(output_path("_github/workflows/ci.yml.template"), ".github/workflows/ci.yml");
/// ```
pub fn output_path(source: &str) -> String {
  let path = source.strip_suffix(".template").unwrap_or(source);
  path
    .split('/')
    .map(|part| match part.strip_prefix('_') {
      Some(rest) => format!(".{rest}"),
      None => part.to_owned(),
    })
    .collect::<Vec<_>>()
    .join("/")
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::BTreeSet;
  use std::path::Path;

  fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).expect("read templates dir") {
      let path = entry.expect("dir entry").path();
      if path.is_dir() {
        walk(root, &path, out);
      } else {
        let rel = path.strip_prefix(root).expect("under root");
        out.insert(rel.to_string_lossy().replace('\\', "/"));
      }
    }
  }

  mod files {
    use super::*;

    #[test]
    fn table_matches_the_templates_directory() {
      let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
      let mut on_disk = BTreeSet::new();
      walk(&root, &root, &mut on_disk);
      let listed: BTreeSet<String> = FILES
        .iter()
        .map(|f| format!("{}/{}", f.dir, f.source))
        .collect();
      assert_eq!(listed, on_disk, "update FILES in src/template.rs");
      assert_eq!(listed.len(), FILES.len(), "a file is listed twice");
    }

    #[test]
    fn every_source_ends_in_dot_template() {
      for f in FILES {
        assert!(f.source.ends_with(".template"), "{}/{}", f.dir, f.source);
      }
    }

    #[test]
    fn every_template_has_the_required_project_files() {
      for template in Template::ALL {
        let paths: BTreeSet<String> = template.files().map(|f| output_path(f.source)).collect();
        for required in [
          "Cargo.toml",
          "README.md",
          "Dockerfile",
          "compose.yaml",
          ".env.example",
          ".github/workflows/ci.yml",
          "src/main.rs",
          "src/lib.rs",
        ] {
          assert!(paths.contains(required), "{template}: missing {required}");
        }
        assert!(
          paths.iter().any(|p| p.starts_with("tests/")),
          "{template}: no tests"
        );
      }
    }
  }

  mod template {
    use super::*;

    #[test]
    fn names_round_trip_through_from_str() {
      for template in Template::ALL {
        assert_eq!(template.name().parse::<Template>(), Ok(template));
        assert_eq!(template.to_string(), template.name());
      }
    }

    #[test]
    fn an_unknown_name_is_an_error_naming_the_choices() {
      let err = "web".parse::<Template>().unwrap_err();
      assert!(err.contains("`web`") && err.contains("cluster"), "{err}");
    }

    #[test]
    fn files_are_common_plus_own() {
      let dirs: BTreeSet<&str> = Template::Cli.files().map(|f| f.dir).collect();
      assert_eq!(dirs, BTreeSet::from([COMMON, "cli"]));
    }
  }

  mod output_path {
    use super::*;

    #[test]
    fn strips_the_suffix_and_maps_underscore_prefixes() {
      assert_eq!(output_path("src/main.rs.template"), "src/main.rs");
      assert_eq!(output_path("_env.example.template"), ".env.example");
      assert_eq!(
        output_path("_github/workflows/ci.yml.template"),
        ".github/workflows/ci.yml"
      );
    }

    #[test]
    fn leaves_other_paths_alone() {
      assert_eq!(output_path("README.md"), "README.md");
      assert_eq!(output_path("a_b/c_d.rs"), "a_b/c_d.rs");
    }
  }
}
