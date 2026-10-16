//! `env`: every key `.env.example` lists is set, in the process environment or in `.env`.
//! An empty value counts as set (`COOKIE_KEY=` means "unset is fine, but say so"); values are
//! never printed.

use super::Check;
use super::project::{Project, Source};
use super::system::System;

const NAME: &str = "env";

pub(crate) fn check(project: &Project, sys: &dyn System) -> Check {
  let Some(example) = &project.example else {
    return Check::skip(NAME, "no .env.example");
  };
  if example.is_empty() {
    return Check::skip(NAME, ".env.example lists no keys");
  }
  let mut missing = Vec::new();
  let mut from_dotenv = 0;
  for (key, _) in example {
    match project.var(sys, key) {
      None => missing.push(key.as_str()),
      Some((_, Source::DotEnv)) => from_dotenv += 1,
      Some(_) => {}
    }
  }
  if !missing.is_empty() {
    let hint = if project.dir.join(".env").is_file() {
      "add them to .env or export them"
    } else {
      "copy .env.example to .env, or export them"
    };
    return Check::fail(
      NAME,
      format!(
        "{} of {} .env.example keys not set: {}; {hint}",
        missing.len(),
        example.len(),
        missing.join(", ")
      ),
    );
  }
  Check::pass(
    NAME,
    format!(
      "all {} .env.example keys set ({from_dotenv} from .env)",
      example.len()
    ),
  )
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::super::Status;
  use super::super::fake::FakeSystem;
  use super::*;

  fn project(example: Option<&str>, dotenv: Option<&str>) -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
    if let Some(text) = example {
      fs::write(tmp.path().join(".env.example"), text).unwrap();
    }
    if let Some(text) = dotenv {
      fs::write(tmp.path().join(".env"), text).unwrap();
    }
    let project = Project::load(tmp.path()).unwrap();
    (tmp, project)
  }

  #[test]
  fn keys_set_in_the_environment_or_dotenv_pass() {
    let (_tmp, project) = project(Some("HOST=x\nPORT=1\nKEY=\n"), Some("PORT=2\nKEY=\n"));
    let check = check(&project, &FakeSystem::default().var("HOST", "h"));
    assert_eq!(check.status, Status::Pass, "{check:?}");
    assert_eq!(check.reason, "all 3 .env.example keys set (2 from .env)");
  }

  #[test]
  fn unset_keys_fail_by_name_without_values() {
    let (_tmp, project) = project(Some("HOST=secret-host\nPORT=1\nAPI_TOKEN=s3cr3t\n"), None);
    let check = check(&project, &FakeSystem::default().var("PORT", "8080"));
    assert_eq!(check.status, Status::Fail);
    assert_eq!(
      check.reason,
      "2 of 3 .env.example keys not set: HOST, API_TOKEN; copy .env.example to .env, or export them"
    );
    assert!(!check.reason.contains("s3cr3t") && !check.reason.contains("secret-host"));
  }

  #[test]
  fn with_a_dotenv_the_hint_says_to_add_to_it() {
    let (_tmp, project) = project(Some("A=1\n"), Some("B=1\n"));
    let check = check(&project, &FakeSystem::default());
    assert!(check.reason.ends_with("add them to .env or export them"));
  }

  #[test]
  fn no_example_or_an_empty_one_skips() {
    let (_tmp, none) = project(None, None);
    assert_eq!(check(&none, &FakeSystem::default()).status, Status::Skip);
    let (_tmp, empty) = project(Some("# nothing\n"), None);
    assert_eq!(check(&empty, &FakeSystem::default()).status, Status::Skip);
  }
}
