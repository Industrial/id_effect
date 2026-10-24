//! The 0.13 batteries through the kit: `process` (F55), `ai`, `embeddings`, `mcp`, `ui`, `i18n`
//! and `admin` resolve to working items under their re-exports and in the prelude.

#![cfg(all(
  feature = "process",
  feature = "embeddings",
  feature = "mcp",
  feature = "admin",
  feature = "i18n"
))]

use id_effect_kit::id_effect::{Never, sync};
use id_effect_kit::prelude::*;
use id_effect_kit::process::{Next, NodeName};

/// A process holding a number: casts add to it, calls read it.
struct Counter(u64);

impl Process for Counter {
  type Args = u64;
  type Env = ();
  type Call = ();
  type Reply = u64;
  type Cast = u64;
  type Info = ();
  type Error = Never;

  fn init(start: u64, _: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Counter(start))
  }

  fn handle_call(self, _: (), _: ProcessCtx) -> Effect<(u64, Next<Self>), Never, ()> {
    sync(move || (self.0, Next::Continue(self)))
  }

  fn handle_cast(self, n: u64, _: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    sync(move || Next::Continue(Counter(self.0 + n)))
  }
}

#[test]
fn kit_reexports_id_effect_process() {
  // The re-export is the crate itself: the mesh's process layer, the same types.
  let name = NodeName::new("kit@local");
  let _node: id_effect_process::ProcessNode = ProcessNode::new(name.clone());
  let _kit: ProcessNode = id_effect_kit::process::ProcessNode::new(name);
  // A behavior written against the kit's names satisfies the crate's trait.
  fn assert_process<P: id_effect_process::Process>() {}
  assert_process::<Counter>();
  let _addr: Option<Addr<Counter>> = None;
}

#[test]
fn breadth_reexports_resolve() {
  // ai: the request types and the tool loop's default budget.
  let request = ChatRequest::user("model", "hello");
  assert_eq!(request.messages.len(), 1);
  assert!(!AiConfig::default().logs_prompts());
  let _budget = ToolLoopBudget::default();
  // embeddings: similarity helpers.
  let same = cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]).expect("same dimension");
  assert!((same - 1.0).abs() < 1e-6);
  // mcp: the server and its default endpoint path.
  let endpoint = McpEndpoint::new(McpServer::new("docs", "1.0.0"));
  assert_eq!(endpoint.path(), id_effect_kit::ai::mcp::DEFAULT_PATH);
  // ui: escaped markup by default (`html!` needs `maud` as a dependency of its own).
  let markup: Markup = "<b>".render();
  assert_eq!(markup.into_string(), "&lt;b&gt;");
  // i18n: locales parse from an allowlist-safe syntax only.
  assert!(Locale::parse("de-AT").is_ok());
  assert!(Locale::parse("../etc").is_err());
  // admin: one permission per entity and action.
  assert_eq!(AdminAction::ALL.len(), 6);
  let _site = AdminSite::new();
}
