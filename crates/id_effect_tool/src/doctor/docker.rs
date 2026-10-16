//! `docker`: a project with a `Dockerfile` has the `docker` CLI, one with a compose file has
//! `docker compose` (or `docker-compose`). Only the CLIs are checked, not whether the daemon
//! runs. Missing ones warn: `cargo run` and `cargo test` do not need them.

use super::Check;
use super::project::Project;
use super::system::System;

const NAME: &str = "docker";

/// Compose file names, in the order `docker compose` looks for them.
const COMPOSE_FILES: [&str; 4] = [
  "compose.yaml",
  "compose.yml",
  "docker-compose.yaml",
  "docker-compose.yml",
];

pub(crate) fn check(project: &Project, sys: &dyn System) -> Check {
  let dockerfile = project.dir.join("Dockerfile").is_file();
  let compose = COMPOSE_FILES
    .into_iter()
    .find(|f| project.dir.join(f).is_file());
  if !dockerfile && compose.is_none() {
    return Check::skip(NAME, "no Dockerfile or compose file");
  }
  let first_line = |program: &str, args: &[&str]| {
    sys
      .run(&project.dir, program, args, None)
      .ok()
      .filter(|out| out.success)
      .map(|out| out.stdout.lines().next().unwrap_or("").trim().to_string())
  };
  let mut found = Vec::new();
  let mut missing = Vec::new();
  if dockerfile {
    match first_line("docker", &["--version"]) {
      Some(version) => found.push(version),
      None => missing.push("Dockerfile present but `docker` is not on PATH".to_string()),
    }
  }
  if let Some(file) = compose {
    match first_line("docker", &["compose", "version"])
      .or_else(|| first_line("docker-compose", &["--version"]))
    {
      Some(version) => found.push(version),
      None => missing.push(format!(
        "{file} present but neither `docker compose` nor `docker-compose` runs"
      )),
    }
  }
  if missing.is_empty() {
    Check::pass(NAME, format!("{} (daemon not checked)", found.join("; ")))
  } else {
    Check::warn(NAME, missing.join("; "))
  }
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::super::Status;
  use super::super::fake::FakeSystem;
  use super::*;

  fn project(files: &[&str]) -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
    for file in files {
      fs::write(tmp.path().join(file), "").unwrap();
    }
    let project = Project::load(tmp.path()).unwrap();
    (tmp, project)
  }

  fn docker() -> FakeSystem {
    FakeSystem::default()
      .ok("docker --version", "Docker version 27.3.1, build ce12230\n")
      .ok("docker compose version", "Docker Compose version v2.29.7\n")
  }

  #[test]
  fn present_tools_pass_with_their_versions() {
    let (_tmp, project) = project(&["Dockerfile", "compose.yaml"]);
    let check = check(&project, &docker());
    assert_eq!(check.status, Status::Pass);
    assert_eq!(
      check.reason,
      "Docker version 27.3.1, build ce12230; Docker Compose version v2.29.7 (daemon not checked)"
    );
  }

  #[test]
  fn a_missing_docker_warns() {
    let (_tmp, project) = project(&["Dockerfile", "docker-compose.yml"]);
    let check = check(&project, &FakeSystem::default());
    assert_eq!(check.status, Status::Warn);
    assert_eq!(
      check.reason,
      "Dockerfile present but `docker` is not on PATH; docker-compose.yml present but neither `docker compose` nor `docker-compose` runs"
    );
  }

  #[test]
  fn standalone_docker_compose_counts() {
    let (_tmp, project) = project(&["compose.yml"]);
    let sys = FakeSystem::default().ok(
      "docker-compose --version",
      "docker-compose version 1.29.2\n",
    );
    let check = check(&project, &sys);
    assert_eq!(check.status, Status::Pass, "{check:?}");
    assert_eq!(
      *sys.ran.borrow(),
      ["docker compose version", "docker-compose --version"]
    );
  }

  #[test]
  fn without_docker_files_the_check_skips() {
    let (_tmp, project) = project(&[]);
    assert_eq!(check(&project, &docker()).status, Status::Skip);
  }
}
