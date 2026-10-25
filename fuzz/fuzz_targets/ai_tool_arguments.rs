//! Tool-call arguments a model produced (id_effect_ai `tools::decode_arguments` and
//! `ToolSet::invoke`, feature `tools`, 0.13 decision D9). A model's output is untrusted (prompt
//! injection), so the arguments are fuzzed JSON and the tool name is picked by the first byte from
//! the registered tools, an unregistered name and a case variant of a registered one.
//!
//! Checks besides "no panic": decoded arguments satisfy the input type's refinements and
//! round-trip through its `Schema`; `invoke` succeeds exactly when the arguments decode for a
//! registered read tool; a handler only ever sees a valid input (it asserts the refinement); an
//! unregistered name and a write tool under the default policy are refused without running
//! anything.
#![no_main]

use std::sync::atomic::{AtomicUsize, Ordering};

use id_effect::{Effect, HasSchema, Schema, Unknown, succeed};
use id_effect_ai::ToolCall;
use id_effect_ai::tools::{
  DEFAULT_MAX_TOOL_OUTPUT_BYTES, Tool, ToolError, ToolPolicy, ToolSet, decode_arguments,
};
use libfuzzer_sys::fuzz_target;

/// A plain tool input: an integer, an optional renamed flag and a list.
#[derive(Clone, Debug, PartialEq, Schema)]
struct Lookup {
  id: i64,
  #[schema("include_archived")]
  archived: Option<bool>,
  tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct DiscountWire {
  percent: i64,
}

/// A refined input: `percent` within `0..=100`, which only the `Schema` refinement enforces.
#[derive(Clone, Debug, PartialEq)]
struct Discount {
  percent: i64,
}

impl HasSchema for Discount {
  type A = Discount;
  type I = Unknown;
  type E = ();

  fn schema() -> Schema<Discount, Unknown, ()> {
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

/// Handler runs (every tool), to check that a refused call ran nothing.
static RUNS: AtomicUsize = AtomicUsize::new(0);

fn tools() -> ToolSet {
  let lookup = Tool::new(
    "lookup",
    "Finds a record",
    |i: Lookup| -> Effect<Lookup, String> {
      RUNS.fetch_add(1, Ordering::SeqCst);
      succeed(i)
    },
  );
  let discount = Tool::new(
    "discount",
    "Applies a discount",
    |d: Discount| -> Effect<DiscountWire, String> {
      RUNS.fetch_add(1, Ordering::SeqCst);
      assert!(
        (0..=100).contains(&d.percent),
        "a handler saw an unrefined input"
      );
      succeed(DiscountWire { percent: d.percent })
    },
  );
  let wipe = Tool::new(
    "wipe",
    "Deletes a record",
    |i: Lookup| -> Effect<Lookup, String> {
      RUNS.fetch_add(1, Ordering::SeqCst);
      succeed(i)
    },
  )
  .writes();
  ToolSet::new()
    .with(lookup)
    .and_then(|s| s.with(discount))
    .and_then(|s| s.with(wipe))
    .expect("valid tools")
}

/// `decode_arguments` for `I`, with the round trip through its `Schema` checked.
fn decodes<I>(tool: &str, arguments: &serde_json::Value) -> bool
where
  I: HasSchema<A = I, I = Unknown> + Clone + PartialEq + std::fmt::Debug,
{
  match decode_arguments::<I>(tool, arguments) {
    Ok(value) => {
      let schema = I::schema();
      let again = schema
        .decode(schema.encode(value.clone()))
        .expect("decoded arguments encode and decode");
      assert_eq!(again, value, "arguments changed through a round trip");
      true
    }
    Err(ToolError::InvalidArguments { tool: t, .. }) => {
      assert_eq!(t, tool);
      false
    }
    Err(other) => panic!("decode_arguments failed with {other:?}"),
  }
}

fuzz_target!(|data: &[u8]| {
  let [pick, json @ ..] = data else {
    return;
  };
  let Ok(arguments) = serde_json::from_slice::<serde_json::Value>(json) else {
    return;
  };
  let lookup_ok = decodes::<Lookup>("lookup", &arguments);
  let discount_ok = decodes::<Discount>("discount", &arguments);
  if let Ok(d) = decode_arguments::<Discount>("discount", &arguments) {
    assert!(
      (0..=100).contains(&d.percent),
      "an unrefined Discount decoded"
    );
  }

  let name = ["lookup", "discount", "wipe", "rm_rf", "Lookup"][usize::from(pick % 5)];
  let call = ToolCall::new("c1", name, arguments);
  let before = RUNS.load(Ordering::SeqCst);
  let result = tools()
    .invoke(&call, &ToolPolicy::default(), DEFAULT_MAX_TOOL_OUTPUT_BYTES)
    .block_on()
    .into_result();
  let ran = RUNS.load(Ordering::SeqCst) - before;
  match name {
    "lookup" | "discount" => {
      let valid = if name == "lookup" {
        lookup_ok
      } else {
        discount_ok
      };
      match &result {
        Ok(output) => {
          assert!(valid, "invalid arguments ran {name}");
          assert_eq!(output.text, output.value.to_string());
          assert!(output.text.len() <= DEFAULT_MAX_TOOL_OUTPUT_BYTES);
        }
        Err(cause) => assert!(
          matches!(
            cause.clone().fail_option(),
            Some(ToolError::InvalidArguments { .. } | ToolError::OutputTooLarge { .. })
          ),
          "{name}: {cause:?}"
        ),
      }
      assert_eq!(ran, usize::from(valid), "{name} ran {ran} times");
    }
    "wipe" => {
      let refused = result.expect_err("a write tool ran under the default policy");
      assert!(matches!(
        refused.fail_option(),
        Some(ToolError::WriteDenied { .. })
      ));
      assert_eq!(ran, 0, "a denied write tool ran");
    }
    _ => {
      let refused = result.expect_err("an unregistered tool ran");
      assert!(matches!(
        refused.fail_option(),
        Some(ToolError::Unregistered { .. })
      ));
      assert_eq!(ran, 0, "an unregistered tool ran");
    }
  }
});
