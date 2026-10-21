//! Typed tools and the D9 rules of `ToolSet::invoke` (ADR 0015, ADR 0013 R1 R4).
#![cfg(feature = "tools")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use id_effect::schema::json_schema_for;
use id_effect::{Effect, HasSchema, Schema, Unknown, fail, succeed};
use id_effect_ai::ToolCall;
use id_effect_ai::tools::{
  DEFAULT_MAX_TOOL_OUTPUT_BYTES, Tool, ToolEffects, ToolError, ToolOutput, ToolPolicy, ToolSet,
  decode_arguments,
};
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Schema)]
struct Lookup {
  id: i64,
  #[schema("include_archived")]
  archived: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Customer {
  name: String,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Deleted {
  ok: bool,
}

/// A refined input: `percent` must be within 0..=100 (a refinement the JSON Schema cannot say).
#[derive(Clone, Debug, PartialEq)]
struct Discount {
  percent: i64,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct DiscountWire {
  percent: i64,
}

impl HasSchema for Discount {
  type A = Discount;
  type I = Unknown;
  type E = ();

  fn schema() -> id_effect::Schema<Discount, Unknown, ()> {
    id_effect::schema::transform(
      id_effect::schema::refine(
        DiscountWire::schema(),
        |d: &DiscountWire| (0..=100).contains(&d.percent),
        "percent must be within 0..=100",
      ),
      |w: DiscountWire| Ok(Discount { percent: w.percent }),
      |d: Discount| DiscountWire { percent: d.percent },
    )
  }
}

/// A primitive input: not a JSON object, so no vendor accepts it as tool parameters.
#[derive(Clone, Debug)]
struct Bare;

impl HasSchema for Bare {
  type A = Bare;
  type I = Unknown;
  type E = ();

  fn schema() -> id_effect::Schema<Bare, Unknown, ()> {
    id_effect::schema::transform(id_effect::schema::i64_unknown_wire(), |_| Ok(Bare), |_| 0)
  }
}

fn counted_lookup(runs: Arc<AtomicUsize>) -> Tool<Lookup, Customer> {
  Tool::new(
    "lookup_customer",
    "Finds a customer by id",
    move |input: Lookup| -> Effect<Customer, String> {
      runs.fetch_add(1, Ordering::SeqCst);
      if input.id == 404 {
        fail("no such customer".to_owned())
      } else {
        succeed(Customer {
          name: format!("customer-{}", input.id),
        })
      }
    },
  )
}

fn delete_tool(runs: Arc<AtomicUsize>) -> Tool<Lookup, Deleted> {
  Tool::new(
    "delete_customer",
    "Deletes a customer",
    move |_: Lookup| -> Effect<Deleted, String> {
      runs.fetch_add(1, Ordering::SeqCst);
      succeed(Deleted { ok: true })
    },
  )
  .writes()
}

fn invoke(set: &ToolSet, call: &ToolCall, policy: ToolPolicy) -> Result<ToolOutput, ToolError> {
  set
    .invoke(call, &policy, DEFAULT_MAX_TOOL_OUTPUT_BYTES)
    .block_on()
    .into_result()
    .map_err(|c| c.fail_option().expect("typed refusal"))
}

#[test]
fn tool_schema_matches_has_schema() {
  let tool = counted_lookup(Arc::default());
  let mut expected = json_schema_for::<Lookup>();
  expected.as_object_mut().expect("object").remove("$schema");
  assert_eq!(Tool::<Lookup, Customer>::parameters_schema(), expected);
  assert_eq!(expected["type"], "object");
  assert_eq!(expected["required"], json!(["id"]));
  assert!(expected["properties"]["include_archived"].is_object());

  let def = tool.definition();
  assert_eq!(def.name, "lookup_customer");
  assert_eq!(def.description, "Finds a customer by id");
  assert_eq!(def.parameters, expected);

  let mut output = json_schema_for::<Customer>();
  output.as_object_mut().expect("object").remove("$schema");
  assert_eq!(Tool::<Lookup, Customer>::output_schema(), output);

  // The set exposes the same definitions, sorted by name.
  let set = ToolSet::new()
    .with(tool)
    .expect("lookup")
    .with(delete_tool(Arc::default()))
    .expect("delete");
  let names: Vec<_> = set.definitions().into_iter().map(|d| d.name).collect();
  assert_eq!(names, ["delete_customer", "lookup_customer"]);
}

#[test]
fn ai_unregistered_tool_call_refused() {
  let runs = Arc::new(AtomicUsize::new(0));
  let set = ToolSet::new()
    .with(counted_lookup(Arc::clone(&runs)))
    .expect("tool");
  let call = ToolCall::new("c1", "drop_database", json!({ "id": 1 }));
  let refused = invoke(&set, &call, ToolPolicy::default().allow_writes()).unwrap_err();
  assert_eq!(
    refused,
    ToolError::Unregistered {
      name: "drop_database".into()
    }
  );
  assert_eq!(runs.load(Ordering::SeqCst), 0, "nothing ran");
  // A huge name is truncated in the refusal, so the model cannot blow up the transcript.
  let long = ToolCall::new("c2", "x".repeat(10_000), json!({}));
  let ToolError::Unregistered { name } = invoke(&set, &long, ToolPolicy::default()).unwrap_err()
  else {
    panic!("unregistered");
  };
  assert!(name.len() < 600);
}

#[test]
fn ai_tool_arguments_schema_decoded() {
  let runs = Arc::new(AtomicUsize::new(0));
  let set = ToolSet::new()
    .with(counted_lookup(Arc::clone(&runs)))
    .expect("tool");
  let policy = ToolPolicy::default();

  for bad in [
    json!({ "id": "1" }),
    json!({}),
    json!("{\"id\": 1}"),
    json!([1]),
    json!({ "id": 1, "include_archived": "yes" }),
  ] {
    let call = ToolCall::new("c", "lookup_customer", bad);
    let refused = invoke(&set, &call, policy).unwrap_err();
    assert!(
      matches!(&refused, ToolError::InvalidArguments { tool, .. } if tool == "lookup_customer"),
      "{refused:?}"
    );
  }
  assert_eq!(
    runs.load(Ordering::SeqCst),
    0,
    "the handler never saw bad input"
  );

  let call = ToolCall::new(
    "c",
    "lookup_customer",
    json!({ "id": 7, "include_archived": true }),
  );
  let out = invoke(&set, &call, policy).expect("valid");
  assert_eq!(out.value, json!({ "name": "customer-7" }));
  assert_eq!(out.text, r#"{"name":"customer-7"}"#);
  assert_eq!(runs.load(Ordering::SeqCst), 1);

  // Refinements run: a value the JSON Schema allows but the Schema refines away is refused.
  let ok: Discount = decode_arguments("discount", &json!({ "percent": 10 })).expect("in range");
  assert_eq!(ok, Discount { percent: 10 });
  let refused = decode_arguments::<Discount>("discount", &json!({ "percent": 400 })).unwrap_err();
  assert!(
    matches!(&refused, ToolError::InvalidArguments { message, .. } if message.contains("0..=100")),
    "{refused:?}"
  );
}

#[test]
fn ai_write_tool_denied_without_policy() {
  let runs = Arc::new(AtomicUsize::new(0));
  let set = ToolSet::new()
    .with(delete_tool(Arc::clone(&runs)))
    .expect("tool");
  assert_eq!(set.effects("delete_customer"), Some(ToolEffects::Write));
  let call = ToolCall::new("c1", "delete_customer", json!({ "id": 1 }));

  let refused = invoke(&set, &call, ToolPolicy::default()).unwrap_err();
  assert_eq!(
    refused,
    ToolError::WriteDenied {
      tool: "delete_customer".into()
    }
  );
  // Denied before decoding: even invalid arguments get the policy refusal, and nothing ran.
  let bad = ToolCall::new("c2", "delete_customer", json!({}));
  assert!(matches!(
    invoke(&set, &bad, ToolPolicy::read_only()),
    Err(ToolError::WriteDenied { .. })
  ));
  assert_eq!(runs.load(Ordering::SeqCst), 0);

  let out = invoke(&set, &call, ToolPolicy::default().allow_writes()).expect("allowed");
  assert_eq!(out.value, json!({ "ok": true }));
  assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn handler_failure_and_output_cap_are_typed() {
  let set = ToolSet::new()
    .with(counted_lookup(Arc::default()))
    .expect("tool");
  let missing = ToolCall::new("c", "lookup_customer", json!({ "id": 404 }));
  assert_eq!(
    invoke(&set, &missing, ToolPolicy::default()).unwrap_err(),
    ToolError::Failed {
      tool: "lookup_customer".into(),
      message: "no such customer".into(),
    }
  );
  assert_eq!(
    invoke(&set, &missing, ToolPolicy::default())
      .unwrap_err()
      .to_string(),
    "tool `lookup_customer` failed: no such customer"
  );
  let call = ToolCall::new("c", "lookup_customer", json!({ "id": 1 }));
  let refused = set
    .invoke(&call, &ToolPolicy::default(), 8)
    .block_on()
    .into_result()
    .unwrap_err()
    .fail_option()
    .expect("typed");
  assert_eq!(
    refused,
    ToolError::OutputTooLarge {
      tool: "lookup_customer".into(),
      max: 8
    }
  );
}

#[test]
fn registration_checks_names_duplicates_and_object_input() {
  let mut set = ToolSet::new();
  assert!(set.is_empty());
  set.register(counted_lookup(Arc::default())).expect("first");
  assert!(matches!(
    set.register(counted_lookup(Arc::default())),
    Err(ToolError::InvalidDefinition(m)) if m.contains("already registered")
  ));
  let bad_name = Tool::new("no spaces", "x", |_: Lookup| -> Effect<Customer, String> {
    succeed(Customer {
      name: String::new(),
    })
  });
  assert!(matches!(
    set.register(bad_name),
    Err(ToolError::InvalidDefinition(m)) if m.contains("must be 1 to 64")
  ));
  let bare = Tool::new("bare", "x", |_: Bare| -> Effect<Customer, String> {
    succeed(Customer {
      name: String::new(),
    })
  });
  assert!(matches!(
    set.register(bare),
    Err(ToolError::InvalidDefinition(m)) if m.contains("JSON object")
  ));
  assert_eq!(set.len(), 1);
  assert!(set.contains("lookup_customer"));
  assert_eq!(set.names(), ["lookup_customer"]);
  assert_eq!(set.effects("nope"), None);
  assert!(format!("{set:?}").contains("lookup_customer"));
}

#[test]
fn tool_accessors_clone_and_direct_call() {
  let tool = delete_tool(Arc::default());
  let copy = tool.clone();
  assert_eq!(copy.name(), "delete_customer");
  assert_eq!(copy.description(), "Deletes a customer");
  assert_eq!(copy.effects(), ToolEffects::Write);
  assert!(format!("{copy:?}").contains("delete_customer"));
  let out = copy
    .call(Lookup {
      id: 1,
      archived: None,
    })
    .block_on()
    .into_result()
    .expect("direct call");
  assert_eq!(out, Deleted { ok: true });
}
