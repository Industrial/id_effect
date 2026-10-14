//! Ex 043 — primary/replica DB providers via `#[named]` variants.

use id_effect::{Effect, Needs, caps, provide, run_with};

type Database = &'static str;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Database, with = || "postgres://primary")]
#[named("primary")]
struct DbPrimaryLive;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Database, with = || "postgres://replica")]
#[named("replica")]
struct DbReplicaLive;

fn app() -> Effect<Database, (), caps!(Database)> {
  Effect::new(|env: &mut caps!(Database)| Ok(*Needs::<Database>::need(env)))
}

fn main() {
  let url = run_with([provide!(DbPrimaryLive)], app()).expect("run");
  assert_eq!(url, "postgres://primary");
  let replica = run_with([provide!(DbReplicaLive)], app()).expect("run replica");
  assert_eq!(replica, "postgres://replica");
  println!("043_named_variant_providers ok: {url}, {replica}");
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::CapabilityGraph;

  #[test]
  fn graph_accepts_both_variants() {
    let graph = CapabilityGraph::new()
      .add(provide!(DbPrimaryLive).0)
      .add(provide!(DbReplicaLive).0);
    assert!(graph.plan().is_ok());
    assert!(graph.diagnostics().is_empty());
  }

  #[test]
  fn primary_variant_selected_in_run_with() {
    let url = run_with([provide!(DbPrimaryLive)], app()).expect("run");
    assert_eq!(url, "postgres://primary");
  }
}
