#![allow(clippy::new_ret_no_self, dead_code)]
//! Print [`CapabilityGraph`] diagnostics; load provider manifests (TOML/JSON).

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use id_effect::{CapabilityGraph, PlannerNode, plan_topological, provide};

#[derive(Parser, Debug)]
#[command(
  name = "id-effect-diagnose",
  about = "Capability graph diagnostics for id_effect"
)]
struct Cli {
  /// Emit machine-readable JSON.
  #[arg(long)]
  json: bool,
  #[command(subcommand)]
  cmd: DiagnoseCmd,
}

#[derive(Subcommand, Debug)]
enum DiagnoseCmd {
  /// Inspect a built-in example graph (`ok`, `missing`, or `cycle`).
  Example {
    #[arg(value_enum, default_value = "missing")]
    kind: ExampleKind,
  },
  /// Run [`CapabilityGraph::diagnostics`] on a small provider list.
  Providers,
  /// Load a provider manifest from TOML or JSON.
  Manifest {
    /// Path to `.toml` or `.json` manifest.
    path: PathBuf,
  },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExampleKind {
  Ok,
  Missing,
  Cycle,
}

#[derive(serde::Deserialize, Debug)]
struct Manifest {
  providers: Vec<ManifestProvider>,
}

#[derive(serde::Deserialize, Debug)]
struct ManifestProvider {
  id: String,
  provides: String,
  #[serde(default)]
  requires: Vec<String>,
  #[serde(default)]
  variant: Option<String>,
}

fn main() -> ExitCode {
  let cli = Cli::parse();
  match cli.cmd {
    DiagnoseCmd::Example { kind } => {
      let code = print_planner_example(kind, cli.json);
      if code == 0 {
        ExitCode::SUCCESS
      } else {
        ExitCode::FAILURE
      }
    }
    DiagnoseCmd::Providers => {
      print_provider_graph(cli.json);
      ExitCode::SUCCESS
    }
    DiagnoseCmd::Manifest { path } => {
      let code = diagnose_manifest(&path, cli.json);
      if code == 0 {
        ExitCode::SUCCESS
      } else {
        ExitCode::FAILURE
      }
    }
  }
}

fn print_json(code: &str, message: &str, suggestion: &str) {
  let payload = serde_json::json!({
    "code": code,
    "message": message,
    "suggestion": suggestion,
  });
  println!("{payload}");
}

fn print_planner_example(kind: ExampleKind, json: bool) -> i32 {
  let err = match kind {
    ExampleKind::Ok => {
      let nodes = vec![
        PlannerNode::new("config", Vec::<&str>::new(), "Config"),
        PlannerNode::new("db", ["Config"], "Database"),
      ];
      match plan_topological(&nodes) {
        Ok(plan) => {
          if json {
            println!(
              "{}",
              serde_json::json!({"ok": true, "build_order": plan.build_order})
            );
          } else {
            println!("ok: build_order = {:?}", plan.build_order);
          }
          return 0;
        }
        Err(e) => e,
      }
    }
    ExampleKind::Missing => {
      let nodes = vec![PlannerNode::new("db", ["Config"], "Database")];
      plan_topological(&nodes).expect_err("missing dependency")
    }
    ExampleKind::Cycle => {
      let nodes = vec![
        PlannerNode::new("a", ["B"], "A"),
        PlannerNode::new("b", ["A"], "B"),
      ];
      plan_topological(&nodes).expect_err("cycle")
    }
  };
  let diag = err.to_diagnostic();
  if json {
    print_json(diag.code, &diag.message, &diag.suggestion);
  } else {
    println!("[{}] {}", diag.code, diag.message);
    println!("  hint: {}", diag.suggestion);
  }
  1
}

fn print_provider_graph(json: bool) {
  #[::id_effect::capability(String)]
  struct ExampleConfig;
  #[::id_effect::capability(String)]
  struct ExampleDb;

  #[derive(::id_effect::ProviderSpecDerive)]
  #[provides(ExampleConfigKey)]
  struct ExampleConfigLive;
  impl ExampleConfigLive {
    fn new() -> String {
      "cfg".into()
    }
  }

  #[derive(::id_effect::ProviderSpecDerive)]
  #[provides(ExampleDbKey)]
  struct ExampleDbLive;
  impl ExampleDbLive {
    fn new() -> String {
      "db".into()
    }
  }

  let graph = CapabilityGraph::new()
    .add(provide!(ExampleConfigLive).0)
    .add(provide!(ExampleDbLive).0);

  emit_graph_report(&graph, json);
}

fn diagnose_manifest(path: &PathBuf, json: bool) -> i32 {
  let raw = fs::read_to_string(path).unwrap_or_else(|e| {
    eprintln!("failed to read {}: {e}", path.display());
    std::process::exit(1);
  });
  let manifest: Manifest = if path.extension().is_some_and(|e| e == "json") {
    serde_json::from_str(&raw).unwrap_or_else(|e| {
      eprintln!("invalid JSON manifest: {e}");
      std::process::exit(1);
    })
  } else {
    toml::from_str(&raw).unwrap_or_else(|e| {
      eprintln!("invalid TOML manifest: {e}");
      std::process::exit(1);
    })
  };

  let nodes: Vec<PlannerNode> = manifest
    .providers
    .iter()
    .map(|p| {
      let provides = match &p.variant {
        Some(v) => format!("{}:{v}", p.provides),
        None => p.provides.clone(),
      };
      PlannerNode::new(p.id.clone(), p.requires.clone(), provides)
    })
    .collect();

  match plan_topological(&nodes) {
    Ok(plan) => {
      if json {
        println!(
          "{}",
          serde_json::json!({"ok": true, "build_order": plan.build_order})
        );
      } else {
        println!("manifest ok: build_order = {:?}", plan.build_order);
      }
      0
    }
    Err(e) => {
      let diag = e.to_diagnostic();
      if json {
        print_json(diag.code, &diag.message, &diag.suggestion);
      } else {
        println!("[{}] {}", diag.code, diag.message);
        println!("  hint: {}", diag.suggestion);
      }
      1
    }
  }
}

fn emit_graph_report(graph: &CapabilityGraph, json: bool) {
  let diags = graph.diagnostics();
  if diags.is_empty() {
    match graph.plan() {
      Ok(order) => {
        if json {
          println!("{}", serde_json::json!({"ok": true, "build_order": order}));
        } else {
          println!("providers plan ok: {order:?}");
        }
      }
      Err(e) => {
        let d = e.to_diagnostic();
        if json {
          print_json(d.code, &d.message, &d.suggestion);
        } else {
          println!("[{}] {}", d.code, d.message);
          println!("  hint: {}", d.suggestion);
        }
      }
    }
  } else {
    for d in diags {
      if json {
        print_json(d.code, &d.message, &d.suggestion);
      } else {
        println!("[{}] {}", d.code, d.message);
        println!("  hint: {}", d.suggestion);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;

  #[test]
  fn manifest_missing_provider_exits_nonzero() {
    let dir = std::env::temp_dir();
    let path = dir.join("id_effect_diagnose_test.toml");
    let mut f = fs::File::create(&path).unwrap();
    write!(
      f,
      r#"
[[providers]]
id = "db"
provides = "Database"
requires = ["Config"]
"#
    )
    .unwrap();
    assert_eq!(diagnose_manifest(&path, false), 1);
    let _ = fs::remove_file(path);
  }

  #[test]
  fn manifest_ok_graph() {
    let dir = std::env::temp_dir();
    let path = dir.join("id_effect_diagnose_ok.json");
    let mut f = fs::File::create(&path).unwrap();
    write!(
      f,
      r#"{{"providers":[
        {{"id":"config","provides":"Config"}},
        {{"id":"db","provides":"Database","requires":["Config"]}}
      ]}}"#
    )
    .unwrap();
    assert_eq!(diagnose_manifest(&path, true), 0);
    let _ = fs::remove_file(path);
  }
  #[test]
  fn print_provider_graph_json_and_text() {
    assert_eq!(print_provider_graph(true), ());
    assert_eq!(print_provider_graph(false), ());
  }

  #[test]
  fn print_planner_examples_all_kinds() {
    use ExampleKind::*;
    assert_eq!(print_planner_example(Ok, false), 0);
    assert_eq!(print_planner_example(Missing, true), 1);
    assert_eq!(print_planner_example(Cycle, false), 1);
  }

  #[test]
  fn emit_graph_report_on_empty_graph() {
    let g = CapabilityGraph::new();
    emit_graph_report(&g, true);
    emit_graph_report(&g, false);
  }
}
