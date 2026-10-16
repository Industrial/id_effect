//! [`LayerGraph`]: an app's layers by name, the dependency edges between them and the
//! capabilities nobody provides, rendered as text or Graphviz DOT (`id-effect doctor --graph`).
//!
//! A [`Layer`] keeps only its build function, so composing layers (`merge`, `provide_to`) records
//! no graph: the app registers each layer it composes, with a name. A layer's capability lists
//! come from its type through [`TypeIds`]: `Out` is what it provides, `In` what it needs. Each
//! need is matched by `TypeId` to every *other* registered layer providing it; an edge runs from
//! the provider to the consumer. A need no registered layer provides is reported **missing**:
//! register the layer that provides it, or ignore it when the app supplies the capability some
//! other way (`provide_all`, a test environment).
//!
//! ```rust
//! use id_effect_kit::graph::{GraphFormat, LayerGraph};
//! use id_effect_kit::id_effect::{HCons, HNil};
//! use id_effect_kit::prelude::*;
//!
//! struct Config;
//! struct Db;
//! struct Smtp;
//! struct Mailer;
//!
//! let config: Layer<HCons<Config, HNil>, ()> = Layer::succeed(hlist![Config]);
//! let db: Layer<HCons<Db, HNil>, (), HCons<Config, HNil>> =
//!   Layer::effect(|| need::<Config>().map(|_| hlist![Db]));
//! let mailer: Layer<HCons<Mailer, HNil>, (), HCons<Config, HCons<Smtp, HNil>>> =
//!   Layer::effect(|| need::<Config>().map(|_| hlist![Mailer]));
//!
//! let graph = LayerGraph::new()
//!   .with("config", &config)
//!   .with("db", &db)
//!   .with("mailer", &mailer);
//! assert_eq!(graph.edges().len(), 2);
//! assert_eq!(graph.missing()[0].capability.short_name(), "Smtp");
//! assert!(graph.to_text().starts_with("layer graph: 3 layers, 2 edges, 1 missing\n"));
//! assert!(graph.render(GraphFormat::Dot).contains("n0 -> n1 [label=\"Config\"];"));
//! ```
//!
//! A binary exposes its graph with [`LayerGraph::describe_if_requested`]: called with the
//! process arguments, it renders the graph when they hold [`DESCRIBE_LAYERS_FLAG`]
//! (`--describe-layers` for text, `--describe-layers=dot` for DOT), which is what
//! `id-effect doctor --graph` runs.

use std::any::TypeId;
use std::fmt::{self, Write as _};

use id_effect::{Layer, TypeIds};

/// The flag a binary answers with its layer graph (see [`LayerGraph::describe_if_requested`]).
pub const DESCRIBE_LAYERS_FLAG: &str = "--describe-layers";

/// A capability: an environment value's `TypeId` and type name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Capability {
  id: TypeId,
  name: &'static str,
}

impl Capability {
  /// The capability `T`.
  pub fn of<T: 'static>() -> Self {
    Self {
      id: TypeId::of::<T>(),
      name: std::any::type_name::<T>(),
    }
  }

  /// Its `TypeId`, which matching uses.
  pub fn id(&self) -> TypeId {
    self.id
  }

  /// Its full type name (`my_app::db::Pool`).
  pub fn type_name(&self) -> &'static str {
    self.name
  }

  /// Its type name without module paths (`Pool`, `Arc<dyn Store>`), which the renderings use.
  pub fn short_name(&self) -> String {
    short_type_name(self.name)
  }
}

/// One registered layer: its name and its capability lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerNode {
  name: String,
  provides: Vec<Capability>,
  needs: Vec<Capability>,
}

impl LayerNode {
  /// The name it was registered with.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// The capabilities it provides (its `Out`), in order.
  pub fn provides(&self) -> &[Capability] {
    &self.provides
  }

  /// The capabilities it needs (its `In`), in order.
  pub fn needs(&self) -> &[Capability] {
    &self.needs
  }
}

/// A dependency: layer `from` provides `capability`, which layer `to` needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge<'g> {
  /// The providing layer's name.
  pub from: &'g str,
  /// The consuming layer's name.
  pub to: &'g str,
  /// The capability passed along.
  pub capability: Capability,
}

/// A need no other registered layer provides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Missing<'g> {
  /// The layer that needs it.
  pub consumer: &'g str,
  /// The capability nobody provides.
  pub capability: Capability,
}

/// How [`LayerGraph::render`] writes the graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GraphFormat {
  /// One block per layer: what it provides, what it needs and from whom ([`LayerGraph::to_text`]).
  #[default]
  Text,
  /// Graphviz DOT ([`LayerGraph::to_dot`]): `dot -Tsvg` draws it.
  Dot,
}

/// The layers an app composes, registered by name; see the [module docs](self).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerGraph {
  nodes: Vec<LayerNode>,
}

impl LayerGraph {
  /// An empty graph.
  pub fn new() -> Self {
    Self::default()
  }

  /// Registers `layer` as `name` (builder form of [`add`](Self::add)).
  pub fn with<Out: TypeIds, E, In: TypeIds>(
    mut self,
    name: impl Into<String>,
    layer: &Layer<Out, E, In>,
  ) -> Self {
    self.add(name, layer);
    self
  }

  /// Registers `layer` as `name`: it provides `Out` and needs `In`. The layer is not built;
  /// only its type is read. Names should be unique, they label the renderings.
  pub fn add<Out: TypeIds, E, In: TypeIds>(
    &mut self,
    name: impl Into<String>,
    _layer: &Layer<Out, E, In>,
  ) -> &mut Self {
    let caps = |list: Vec<(TypeId, &'static str)>| {
      list
        .into_iter()
        .map(|(id, name)| Capability { id, name })
        .collect()
    };
    self.nodes.push(LayerNode {
      name: name.into(),
      provides: caps(Out::type_ids()),
      needs: caps(In::type_ids()),
    });
    self
  }

  /// The registered layers, in registration order.
  pub fn layers(&self) -> &[LayerNode] {
    &self.nodes
  }

  /// Every dependency: per consumer (registration order), per need, per provider.
  pub fn edges(&self) -> Vec<Edge<'_>> {
    let mut edges = Vec::new();
    for (to, node) in self.nodes.iter().enumerate() {
      for need in &node.needs {
        for from in self.providers(to, need) {
          edges.push(Edge {
            from: &self.nodes[from].name,
            to: &node.name,
            capability: *need,
          });
        }
      }
    }
    edges
  }

  /// Every need no other registered layer provides, per consumer and need.
  pub fn missing(&self) -> Vec<Missing<'_>> {
    let mut missing = Vec::new();
    for (to, node) in self.nodes.iter().enumerate() {
      for need in &node.needs {
        if self.providers(to, need).next().is_none() {
          missing.push(Missing {
            consumer: &node.name,
            capability: *need,
          });
        }
      }
    }
    missing
  }

  /// The graph in `format`.
  pub fn render(&self, format: GraphFormat) -> String {
    match format {
      GraphFormat::Text => self.to_text(),
      GraphFormat::Dot => self.to_dot(),
    }
  }

  /// The graph as text: a summary line (`layer graph: 3 layers, 2 edges, 1 missing`), then per
  /// layer what it provides and, per need, the layers providing it or `MISSING`.
  pub fn to_text(&self) -> String {
    let mut out = self.summary();
    out.push('\n');
    for (index, node) in self.nodes.iter().enumerate() {
      let _ = writeln!(out, "{}", node.name);
      let provides = if node.provides.is_empty() {
        "(nothing)".to_string()
      } else {
        join_short(&node.provides)
      };
      let _ = writeln!(out, "  provides {provides}");
      for need in &node.needs {
        let from: Vec<&str> = self
          .providers(index, need)
          .map(|i| self.nodes[i].name.as_str())
          .collect();
        let from = if from.is_empty() {
          "MISSING (no registered layer provides it)".to_string()
        } else {
          from.join(", ")
        };
        let _ = writeln!(out, "  needs    {} <- {from}", need.short_name());
      }
    }
    out
  }

  /// The graph as Graphviz DOT: a `// layer graph: …` summary comment, a box per layer (`n0`,
  /// `n1`, … in registration order, labelled with its name), a dashed red ellipse per missing
  /// capability (`m0`, …) and an edge from provider to consumer labelled with the capability.
  pub fn to_dot(&self) -> String {
    let mut out = format!("// {}\ndigraph layers {{\n", self.summary());
    out.push_str("  rankdir=LR;\n  node [shape=box];\n");
    for (index, node) in self.nodes.iter().enumerate() {
      let _ = writeln!(out, "  n{index} [label={}];", dot_string(&node.name));
    }
    let mut absent: Vec<Capability> = Vec::new();
    for missing in self.missing() {
      if !absent.iter().any(|c| c.id == missing.capability.id) {
        absent.push(missing.capability);
      }
    }
    for (index, cap) in absent.iter().enumerate() {
      let _ = writeln!(
        out,
        "  m{index} [label={}, shape=ellipse, style=dashed, color=red];",
        dot_string(&format!("{} (missing)", cap.short_name()))
      );
    }
    for (to, node) in self.nodes.iter().enumerate() {
      for need in &node.needs {
        let label = dot_string(&need.short_name());
        let mut provided = false;
        for from in self.providers(to, need) {
          provided = true;
          let _ = writeln!(out, "  n{from} -> n{to} [label={label}];");
        }
        if !provided && let Some(m) = absent.iter().position(|c| c.id == need.id) {
          let _ = writeln!(
            out,
            "  m{m} -> n{to} [label={label}, style=dashed, color=red];"
          );
        }
      }
    }
    out.push_str("}\n");
    out
  }

  /// The rendered graph when `args` (the process arguments) hold [`DESCRIBE_LAYERS_FLAG`]:
  /// `--describe-layers=dot` renders DOT, `--describe-layers` (or any other value) text.
  /// `None` otherwise, so `main` goes on to run the app.
  ///
  /// ```rust
  /// use id_effect_kit::graph::LayerGraph;
  ///
  /// let graph = LayerGraph::new();
  /// assert!(graph.describe_if_requested(["app"]).is_none());
  /// let text = graph.describe_if_requested(["app", "--describe-layers"]).unwrap();
  /// assert_eq!(text, "layer graph: 0 layers, 0 edges, 0 missing\n");
  /// let dot = graph.describe_if_requested(["app", "--describe-layers=dot"]).unwrap();
  /// assert!(dot.contains("digraph layers {"));
  /// ```
  pub fn describe_if_requested<I, S>(&self, args: I) -> Option<String>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
  {
    args.into_iter().find_map(|arg| {
      let rest = arg.as_ref().strip_prefix(DESCRIBE_LAYERS_FLAG)?;
      match rest {
        "=dot" => Some(self.to_dot()),
        "" => Some(self.to_text()),
        _ if rest.starts_with('=') => Some(self.to_text()),
        _ => None,
      }
    })
  }

  /// `layer graph: N layers, N edges, N missing`.
  fn summary(&self) -> String {
    format!(
      "layer graph: {} layers, {} edges, {} missing",
      self.nodes.len(),
      self.edges().len(),
      self.missing().len()
    )
  }

  /// The indices of the layers other than `consumer` providing `need`.
  fn providers<'a>(
    &'a self,
    consumer: usize,
    need: &'a Capability,
  ) -> impl Iterator<Item = usize> + 'a {
    self
      .nodes
      .iter()
      .enumerate()
      .filter(move |(i, node)| *i != consumer && node.provides.iter().any(|c| c.id == need.id))
      .map(|(i, _)| i)
  }
}

impl fmt::Display for LayerGraph {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.to_text())
  }
}

fn join_short(caps: &[Capability]) -> String {
  caps
    .iter()
    .map(Capability::short_name)
    .collect::<Vec<_>>()
    .join(", ")
}

/// `name` without module paths, generics included: `alloc::sync::Arc<dyn a::B>` → `Arc<dyn B>`.
fn short_type_name(name: &str) -> String {
  let mut out = String::with_capacity(name.len());
  let mut segment_start = 0;
  let mut chars = name.chars().peekable();
  while let Some(c) = chars.next() {
    if c == ':' && chars.peek() == Some(&':') {
      chars.next();
      out.truncate(segment_start);
    } else {
      out.push(c);
      if !(c.is_alphanumeric() || c == '_') {
        segment_start = out.len();
      }
    }
  }
  out
}

/// `s` as a quoted DOT string.
fn dot_string(s: &str) -> String {
  let mut out = String::with_capacity(s.len() + 2);
  out.push('"');
  for c in s.chars() {
    match c {
      '"' | '\\' => {
        out.push('\\');
        out.push(c);
      }
      '\n' => out.push_str("\\n"),
      _ => out.push(c),
    }
  }
  out.push('"');
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{HCons, HNil, hlist, need};

  struct Config;
  struct Db;
  struct Smtp;
  struct Mailer;

  type One<T> = HCons<T, HNil>;

  fn config() -> Layer<One<Config>, ()> {
    Layer::succeed(hlist![Config])
  }

  fn db() -> Layer<One<Db>, (), One<Config>> {
    Layer::effect(|| need::<Config>().map(|_| hlist![Db]))
  }

  fn mailer() -> Layer<One<Mailer>, (), HCons<Config, One<Smtp>>> {
    Layer::effect(|| need::<Config>().map(|_| hlist![Mailer]))
  }

  fn app() -> LayerGraph {
    LayerGraph::new()
      .with("config", &config())
      .with("db", &db())
      .with("mailer", &mailer())
  }

  #[test]
  fn short_names_drop_module_paths_inside_generics_too() {
    assert_eq!(short_type_name("a::b::Pool"), "Pool");
    assert_eq!(
      short_type_name("alloc::sync::Arc<dyn my_app::store::Store + core::marker::Send>"),
      "Arc<dyn Store + Send>"
    );
    assert_eq!(short_type_name("(u8, a::B)"), "(u8, B)");
    assert_eq!(Capability::of::<Db>().short_name(), "Db");
    assert!(Capability::of::<Db>().type_name().ends_with("tests::Db"));
  }

  #[test]
  fn registered_layers_keep_their_capability_lists() {
    let graph = app();
    let names: Vec<&str> = graph.layers().iter().map(LayerNode::name).collect();
    assert_eq!(names, ["config", "db", "mailer"]);
    let mailer = &graph.layers()[2];
    assert_eq!(mailer.provides(), [Capability::of::<Mailer>()]);
    assert_eq!(
      mailer.needs(),
      [Capability::of::<Config>(), Capability::of::<Smtp>()]
    );
  }

  #[test]
  fn needs_are_matched_to_providers_and_the_rest_reported_missing() {
    let graph = app();
    let config = Capability::of::<Config>();
    assert_eq!(
      graph.edges(),
      [
        Edge {
          from: "config",
          to: "db",
          capability: config
        },
        Edge {
          from: "config",
          to: "mailer",
          capability: config
        },
      ]
    );
    assert_eq!(
      graph.missing(),
      [Missing {
        consumer: "mailer",
        capability: Capability::of::<Smtp>()
      }]
    );
  }

  #[test]
  fn a_layer_does_not_provide_its_own_need() {
    let looped: Layer<One<Config>, (), One<Config>> =
      Layer::effect(|| need::<Config>().map(|_| hlist![Config]));
    let graph = LayerGraph::new().with("loop", &looped);
    assert!(graph.edges().is_empty());
    assert_eq!(graph.missing().len(), 1);
  }

  #[test]
  fn every_provider_of_a_need_gets_an_edge() {
    let mut graph = LayerGraph::new();
    graph
      .add("primary", &config())
      .add("fallback", &config())
      .add("db", &db());
    let from: Vec<&str> = graph.edges().iter().map(|e| e.from).collect();
    assert_eq!(from, ["primary", "fallback"]);
    assert!(
      graph
        .to_text()
        .contains("needs    Config <- primary, fallback")
    );
  }

  #[test]
  fn text_lists_providers_and_flags_missing_needs() {
    let want = "\
layer graph: 3 layers, 2 edges, 1 missing
config
  provides Config
db
  provides Db
  needs    Config <- config
mailer
  provides Mailer
  needs    Config <- config
  needs    Smtp <- MISSING (no registered layer provides it)
";
    assert_eq!(app().to_text(), want);
    assert_eq!(app().to_string(), want);
    assert_eq!(app().render(GraphFormat::Text), want);
  }

  #[test]
  fn a_layer_providing_nothing_says_so() {
    let unit: Layer<HNil, ()> = Layer::succeed(HNil);
    assert!(
      LayerGraph::new()
        .with("noop", &unit)
        .to_text()
        .contains("  provides (nothing)\n")
    );
  }

  #[test]
  fn dot_strings_are_escaped() {
    assert_eq!(dot_string("a\"b\\c\nd"), r#""a\"b\\c\nd""#);
    let graph = LayerGraph::new().with("say \"hi\"", &config());
    assert!(graph.to_dot().contains(r#"n0 [label="say \"hi\""];"#));
  }

  #[test]
  fn describe_if_requested_reads_the_flag() {
    let graph = app();
    assert_eq!(graph.describe_if_requested(["app", "serve"]), None);
    assert_eq!(
      graph.describe_if_requested(["app", "--describe-layersx"]),
      None
    );
    assert_eq!(
      graph.describe_if_requested(["app", "--describe-layers"]),
      Some(graph.to_text())
    );
    assert_eq!(
      graph.describe_if_requested(["app", "--describe-layers=text"]),
      Some(graph.to_text())
    );
    assert_eq!(
      graph.describe_if_requested(vec!["app".to_string(), "--describe-layers=dot".to_string()]),
      Some(graph.to_dot())
    );
  }
}
