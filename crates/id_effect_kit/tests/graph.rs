//! `LayerGraph` over real layers: a missing provider, and the DOT rendering against a golden file.

use id_effect_kit::graph::{Capability, GraphFormat, LayerGraph};
use id_effect_kit::id_effect::{HCons, HNil};
use id_effect_kit::prelude::*;

struct Config;
struct Pool;
struct Users;
struct Clock;
struct Mailer;

type One<A> = HCons<A, HNil>;
type Two<A, B> = HCons<A, One<B>>;

fn config() -> Layer<One<Config>, ()> {
  Layer::succeed(hlist![Config])
}

fn pool() -> Layer<One<Pool>, (), One<Config>> {
  Layer::effect(|| need::<Config>().map(|_| hlist![Pool]))
}

fn users() -> Layer<One<Users>, (), Two<Pool, Clock>> {
  Layer::effect(|| need::<Pool>().map(|_| hlist![Users]))
}

fn mailer() -> Layer<One<Mailer>, (), Two<Config, Clock>> {
  Layer::effect(|| need::<Config>().map(|_| hlist![Mailer]))
}

/// `Clock` is needed by `users` and `mailer` and provided by nobody.
fn app() -> LayerGraph {
  LayerGraph::new()
    .with("config", &config())
    .with("pool", &pool())
    .with("users", &users())
    .with("mailer", &mailer())
}

#[test]
fn a_capability_nobody_provides_is_reported_for_each_consumer() {
  let graph = app();
  let missing: Vec<(&str, Capability)> = graph
    .missing()
    .iter()
    .map(|m| (m.consumer, m.capability))
    .collect();
  let clock = Capability::of::<Clock>();
  assert_eq!(missing, [("users", clock), ("mailer", clock)]);
  let text = graph.to_text();
  assert!(
    text.starts_with("layer graph: 4 layers, 3 edges, 2 missing\n"),
    "{text}"
  );
  assert!(text.contains("  needs    Clock <- MISSING"), "{text}");
}

#[test]
fn a_complete_graph_has_nothing_missing() {
  let graph = LayerGraph::new()
    .with("config", &config())
    .with("pool", &pool());
  assert!(graph.missing().is_empty());
  assert_eq!(graph.edges().len(), 1);
}

#[test]
fn dot_matches_the_golden_file() {
  let golden = include_str!("golden/layers.dot");
  assert_eq!(app().render(GraphFormat::Dot), golden);
}
