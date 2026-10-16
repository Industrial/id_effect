//! `graph` (`--graph`): builds the project, runs its binary with `--describe-layers` and keeps
//! the layer graph it prints (`id_effect_kit::graph::LayerGraph::describe_if_requested`). The
//! graph's summary line (`layer graph: N layers, N edges, N missing`) decides the outcome.

use std::time::Duration;

use super::project::Project;
use super::system::{RunError, RunOutput, System};
use super::{Check, GraphFormat};

const NAME: &str = "graph";

/// How long the built binary may take to print its graph; one that ignores the flag starts
/// the app instead and is stopped here.
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(60);

/// The flag the binary answers (`id_effect_kit::graph::DESCRIBE_LAYERS_FLAG`).
const FLAG: &str = "--describe-layers";

pub(crate) fn check(
  project: &Project,
  sys: &dyn System,
  format: GraphFormat,
) -> (Check, Option<String>) {
  match sys.run(&project.dir, "cargo", &["build", "--quiet"], None) {
    Ok(out) if out.success => {}
    Ok(out) => {
      return (
        Check::fail(NAME, format!("`cargo build` failed: {}", error_line(&out))),
        None,
      );
    }
    Err(RunError::Spawn(reason)) => return (Check::fail(NAME, reason), None),
    Err(RunError::TimedOut(_)) => return (Check::fail(NAME, "`cargo build` timed out"), None),
  }
  let flag = match format {
    GraphFormat::Text => FLAG.to_string(),
    GraphFormat::Dot => format!("{FLAG}=dot"),
  };
  let run = sys.run(
    &project.dir,
    "cargo",
    &["run", "--quiet", "--", &flag],
    Some(DESCRIBE_TIMEOUT),
  );
  let out = match run {
    Ok(out) if out.success => out.stdout,
    Ok(out) => {
      let reason = format!("`cargo run -- {flag}` failed: {}", error_line(&out));
      return (Check::fail(NAME, reason), None);
    }
    Err(RunError::Spawn(reason)) => return (Check::fail(NAME, reason), None),
    Err(RunError::TimedOut(after)) => {
      let reason = format!(
        "the binary was still running {}s after `{flag}` and was stopped: it does not answer the flag (see id_effect_kit::graph)",
        after.as_secs()
      );
      return (Check::warn(NAME, reason), None);
    }
  };
  let Some((summary, missing)) = summary(&out) else {
    let reason = format!(
      "the binary printed no layer graph for `{flag}`; register its layers in an id_effect_kit::graph::LayerGraph and call describe_if_requested"
    );
    return (Check::warn(NAME, reason), None);
  };
  let check = if missing == 0 {
    Check::pass(NAME, summary)
  } else {
    Check::warn(
      NAME,
      format!("{summary}: capabilities no registered layer provides (below)"),
    )
  };
  (check, Some(out))
}

/// The `N layers, N edges, N missing` part of the summary line, and the missing count.
fn summary(out: &str) -> Option<(String, usize)> {
  let line = out.lines().find_map(|line| {
    line
      .split_once("layer graph: ")
      .map(|(_, rest)| rest.trim())
  })?;
  let missing = line
    .split(", ")
    .find_map(|part| part.strip_suffix(" missing"))?
    .parse()
    .ok()?;
  Some((line.to_string(), missing))
}

/// The first `error` line of stderr, else its last non-empty line.
fn error_line(out: &RunOutput) -> String {
  let lines = || out.stderr.lines().map(str::trim).filter(|l| !l.is_empty());
  lines()
    .find(|l| l.starts_with("error"))
    .or_else(|| lines().next_back())
    .unwrap_or("no output")
    .to_string()
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::super::Status;
  use super::super::fake::FakeSystem;
  use super::*;

  const GRAPH: &str =
    "layer graph: 2 layers, 1 edges, 0 missing\nenv\n  provides ConfigProviderService\n";

  fn project() -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
    let project = Project::load(tmp.path()).unwrap();
    (tmp, project)
  }

  fn built() -> FakeSystem {
    FakeSystem::default().ok("cargo build --quiet", "")
  }

  fn failed(stderr: &str) -> Result<RunOutput, RunError> {
    Ok(RunOutput {
      success: false,
      stdout: String::new(),
      stderr: stderr.into(),
    })
  }

  #[test]
  fn summaries_parse_from_text_and_dot() {
    assert_eq!(
      summary(GRAPH),
      Some(("2 layers, 1 edges, 0 missing".into(), 0))
    );
    assert_eq!(
      summary("// layer graph: 4 layers, 3 edges, 2 missing\ndigraph layers {\n}\n"),
      Some(("4 layers, 3 edges, 2 missing".into(), 2))
    );
    assert_eq!(summary("listening on 0.0.0.0:8080\n"), None);
  }

  #[test]
  fn a_complete_graph_passes_and_is_kept() {
    let (_tmp, project) = project();
    let sys = built().ok("cargo run --quiet -- --describe-layers", GRAPH);
    let (check, graph) = super::check(&project, &sys, GraphFormat::Text);
    assert_eq!(check.status, Status::Pass);
    assert_eq!(check.reason, "2 layers, 1 edges, 0 missing");
    assert_eq!(graph.as_deref(), Some(GRAPH));
  }

  #[test]
  fn missing_capabilities_warn_and_dot_asks_for_dot() {
    let (_tmp, project) = project();
    let dot = "// layer graph: 1 layers, 0 edges, 1 missing\ndigraph layers {\n}\n";
    let sys = built().ok("cargo run --quiet -- --describe-layers=dot", dot);
    let (check, graph) = super::check(&project, &sys, GraphFormat::Dot);
    assert_eq!(check.status, Status::Warn);
    assert!(check.reason.starts_with("1 layers, 0 edges, 1 missing: "));
    assert_eq!(graph.as_deref(), Some(dot));
  }

  #[test]
  fn a_binary_without_the_hook_warns() {
    let (_tmp, project) = project();
    let silent = built().ok("cargo run --quiet -- --describe-layers", "hello\n");
    let (check, graph) = super::check(&project, &silent, GraphFormat::Text);
    assert_eq!(check.status, Status::Warn);
    assert!(graph.is_none());
    let serving = built().answer(
      "cargo run --quiet -- --describe-layers",
      Err(RunError::TimedOut(DESCRIBE_TIMEOUT)),
    );
    let (check, _) = super::check(&project, &serving, GraphFormat::Text);
    assert_eq!(check.status, Status::Warn);
    assert!(check.reason.contains("still running 60s"), "{check:?}");
  }

  #[test]
  fn build_and_run_failures_fail_with_the_error() {
    let (_tmp, project) = project();
    let broken = FakeSystem::default().answer(
      "cargo build --quiet",
      failed("warning: x\nerror[E0425]: cannot find value `y`\n  --> src/main.rs\n"),
    );
    let (check, graph) = super::check(&project, &broken, GraphFormat::Text);
    assert_eq!(check.status, Status::Fail);
    assert_eq!(
      check.reason,
      "`cargo build` failed: error[E0425]: cannot find value `y`"
    );
    assert!(graph.is_none());
    let crashing = built().answer(
      "cargo run --quiet -- --describe-layers",
      failed("thread 'main' panicked\n"),
    );
    let (check, _) = super::check(&project, &crashing, GraphFormat::Text);
    assert_eq!(check.status, Status::Fail);
    assert!(check.reason.ends_with("failed: thread 'main' panicked"));
    let (check, _) = super::check(&project, &FakeSystem::default(), GraphFormat::Text);
    assert_eq!(check.reason, "`cargo` not found on PATH");
  }
}
