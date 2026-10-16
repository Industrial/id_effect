//! `rustc`: the compiler is at least the project's `rust-version` and the id_effect MSRV.

use std::fs;

use super::Check;
use super::project::{Project, toml_entries, unquote};
use super::system::{RunError, System};

const NAME: &str = "rustc";

/// The MSRV of the id_effect release this tool ships with (the workspace `rust-version`).
pub(crate) const ID_EFFECT_MSRV: &str = env!("CARGO_PKG_RUST_VERSION");

/// Runs `$RUSTC --version` (default `rustc`) in the project, so a `rust-toolchain.toml` applies.
pub(crate) fn check(project: &Project, sys: &dyn System) -> Check {
  let rustc = sys.var("RUSTC").unwrap_or_else(|| "rustc".into());
  let output = match sys.run(&project.dir, &rustc, &["--version"], None) {
    Ok(out) if out.success => out.stdout,
    Ok(out) => {
      let detail = out.stderr.lines().next().unwrap_or("no output");
      return Check::fail(NAME, format!("`{rustc} --version` failed: {detail}"));
    }
    Err(RunError::Spawn(reason)) => return Check::fail(NAME, reason),
    Err(RunError::TimedOut(_)) => return Check::fail(NAME, format!("`{rustc} --version` hung")),
  };
  let Some((shown, have)) = rustc_version(&output) else {
    let line = output.lines().next().unwrap_or("").trim();
    return Check::warn(NAME, format!("cannot read a version from `{line}`"));
  };
  let project_req = project_rust_version(project);
  let mut needs = Vec::new();
  if let Some(req) = &project_req {
    needs.push((req.clone(), "project rust-version"));
  }
  needs.push((ID_EFFECT_MSRV.to_string(), "id_effect MSRV"));
  for (req, what) in &needs {
    match parse_version(req) {
      Some(min) if have < min => {
        return Check::fail(
          NAME,
          format!("rustc {shown} is older than {req} ({what}); run `rustup update`"),
        );
      }
      Some(_) => {}
      None => return Check::warn(NAME, format!("cannot read the {what} `{req}`")),
    }
  }
  let detail = needs
    .iter()
    .map(|(req, what)| format!("{what} {req}"))
    .collect::<Vec<_>>()
    .join(", ");
  let none = if project_req.is_none() {
    "; the project sets no rust-version"
  } else {
    ""
  };
  Check::pass(NAME, format!("rustc {shown} >= {detail}{none}"))
}

/// The project's `rust-version`: `[package]`, else `[workspace.package]`, following
/// `rust-version.workspace = true` up to the first ancestor manifest declaring one.
fn project_rust_version(project: &Project) -> Option<String> {
  let declared = |manifest: &str, tables: &[&str]| {
    toml_entries(manifest)
      .into_iter()
      .filter(|e| tables.contains(&e.table.as_str()))
      .find(|e| e.key == "rust-version")
      .and_then(|e| unquote(&e.value))
  };
  if let Some(version) = declared(&project.manifest, &["package", "workspace.package"]) {
    return Some(version);
  }
  let inherits = toml_entries(&project.manifest).into_iter().any(|e| {
    e.table == "package"
      && (e.key == "rust-version.workspace"
        || (e.key == "rust-version" && e.value.contains("workspace")))
  });
  if !inherits {
    return None;
  }
  project.dir.ancestors().skip(1).find_map(|dir| {
    let manifest = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    declared(&manifest, &["workspace.package"])
  })
}

/// `rustc 1.99.0-nightly (… 2026-07-10)` → (`1.99.0-nightly`, (1, 99, 0)).
fn rustc_version(output: &str) -> Option<(String, (u64, u64, u64))> {
  let shown = output.split_whitespace().nth(1)?;
  let version = parse_version(shown)?;
  Some((shown.to_string(), version))
}

/// `1.94`, `1.94.0` or `1.94.0-nightly` → (1, 94, 0); the pre-release tag is ignored.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
  let core = s.split(['-', '+']).next()?;
  let mut parts = core.split('.');
  let major = parts.next()?.parse().ok()?;
  let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
  let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
  parts.next().is_none().then_some((major, minor, patch))
}

#[cfg(test)]
mod tests {
  use super::super::Status;
  use super::super::fake::FakeSystem;
  use super::super::system::RunOutput;
  use super::*;

  fn project(manifest: &str) -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Cargo.toml"), manifest).unwrap();
    let project = Project::load(tmp.path()).unwrap();
    (tmp, project)
  }

  fn rustc(version: &str) -> FakeSystem {
    FakeSystem::default().ok(
      "rustc --version",
      &format!("rustc {version} (abc 2026-01-01)\n"),
    )
  }

  #[test]
  fn versions_parse_with_and_without_patch_and_tag() {
    assert_eq!(parse_version("1.94"), Some((1, 94, 0)));
    assert_eq!(parse_version("1.94.2"), Some((1, 94, 2)));
    assert_eq!(parse_version("1.99.0-nightly"), Some((1, 99, 0)));
    assert_eq!(parse_version("one"), None);
    assert_eq!(parse_version("1.2.3.4"), None);
    assert_eq!(
      rustc_version("rustc 1.95.0 (x y)"),
      Some(("1.95.0".into(), (1, 95, 0)))
    );
  }

  #[test]
  fn a_new_enough_rustc_passes_and_names_both_requirements() {
    let (_tmp, project) = project("[package]\nname = \"a\"\nrust-version = \"1.80\"\n");
    let check = check(&project, &rustc("1.200.0"));
    assert_eq!(check.status, Status::Pass, "{check:?}");
    assert_eq!(
      check.reason,
      format!("rustc 1.200.0 >= project rust-version 1.80, id_effect MSRV {ID_EFFECT_MSRV}")
    );
  }

  #[test]
  fn an_old_rustc_fails_against_the_project_rust_version() {
    let (_tmp, project) = project("[package]\nname = \"a\"\nrust-version = \"1.300\"\n");
    let check = check(&project, &rustc("1.200.0"));
    assert_eq!(check.status, Status::Fail);
    assert!(
      check
        .reason
        .starts_with("rustc 1.200.0 is older than 1.300 (project rust-version)"),
      "{check:?}"
    );
  }

  #[test]
  fn an_old_rustc_fails_against_the_id_effect_msrv() {
    let (_tmp, project) = project("[package]\nname = \"a\"\n");
    let check = check(&project, &rustc("1.0.0"));
    assert_eq!(check.status, Status::Fail);
    assert!(check.reason.contains("(id_effect MSRV)"), "{check:?}");
  }

  #[test]
  fn an_inherited_rust_version_is_read_from_the_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
      tmp.path().join("Cargo.toml"),
      "[workspace]\nmembers = [\"app\"]\n[workspace.package]\nrust-version = \"1.300\"\n",
    )
    .unwrap();
    let app = tmp.path().join("app");
    fs::create_dir(&app).unwrap();
    fs::write(
      app.join("Cargo.toml"),
      "[package]\nname = \"app\"\nrust-version.workspace = true\n",
    )
    .unwrap();
    let project = Project::load(&app).unwrap();
    assert_eq!(project_rust_version(&project).as_deref(), Some("1.300"));
    assert_eq!(check(&project, &rustc("1.200.0")).status, Status::Fail);
  }

  #[test]
  fn without_a_project_rust_version_only_the_msrv_counts() {
    let (_tmp, project) = project("[package]\nname = \"a\"\n");
    let check = check(&project, &rustc("1.200.0"));
    assert_eq!(check.status, Status::Pass);
    assert!(check.reason.ends_with("the project sets no rust-version"));
  }

  #[test]
  fn a_missing_or_broken_rustc_fails_and_odd_output_warns() {
    let (_tmp, project) = project("[package]\nname = \"a\"\n");
    let missing = check(&project, &FakeSystem::default());
    assert_eq!(missing.status, Status::Fail);
    assert_eq!(missing.reason, "`rustc` not found on PATH");
    let broken = FakeSystem::default().answer(
      "rustc --version",
      Ok(RunOutput {
        success: false,
        stdout: String::new(),
        stderr: "error: toolchain 'x' is not installed\n".into(),
      }),
    );
    let broken = check(&project, &broken);
    assert_eq!(broken.status, Status::Fail);
    assert!(broken.reason.contains("toolchain 'x' is not installed"));
    let odd = FakeSystem::default().ok("rustc --version", "what\n");
    assert_eq!(check(&project, &odd).status, Status::Warn);
  }

  #[test]
  fn rustc_comes_from_the_rustc_variable() {
    let (_tmp, project) = project("[package]\nname = \"a\"\n");
    let sys = FakeSystem::default()
      .var("RUSTC", "/opt/rustc")
      .ok("/opt/rustc --version", "rustc 1.200.0 (x)\n");
    assert_eq!(check(&project, &sys).status, Status::Pass);
  }
}
