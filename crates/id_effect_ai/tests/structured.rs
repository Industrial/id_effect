//! Structured output decoded through `Schema` (ai.llm #4): refinements run, a decode failure is
//! typed, and the repair retry is bounded.
#![cfg(all(feature = "tools", feature = "test-util"))]

use id_effect::{HCons, HNil, HasSchema, Schema, Unknown};
use id_effect_ai::structured::{
  MAX_REPAIR_RETRIES, StructuredError, StructuredOptions, complete_structured,
  complete_structured_with, decode_reply, schema_instruction,
};
use id_effect_ai::testing::ScriptedModel;
use id_effect_ai::{AiError, ChatRequest, ChatResponse, ChatRole, LanguageModelService, Usage};

type Env = HCons<LanguageModelService, HNil>;

#[derive(Clone, Debug, PartialEq, Schema)]
struct InvoiceWire {
  number: String,
  total_cents: i64,
}

/// An invoice whose total must be positive: a refinement on top of the derived schema.
#[derive(Clone, Debug, PartialEq)]
struct Invoice {
  number: String,
  total_cents: i64,
}

impl HasSchema for Invoice {
  type A = Invoice;
  type I = Unknown;
  type E = ();

  fn schema() -> id_effect::Schema<Invoice, Unknown, ()> {
    id_effect::schema::transform(
      id_effect::schema::refine(
        InvoiceWire::schema(),
        |w: &InvoiceWire| w.total_cents > 0,
        "total_cents must be positive",
      ),
      |w: InvoiceWire| {
        Ok(Invoice {
          number: w.number,
          total_cents: w.total_cents,
        })
      },
      |i: Invoice| InvoiceWire {
        number: i.number,
        total_cents: i.total_cents,
      },
    )
  }
}

fn ask(model: &ScriptedModel) -> Result<Invoice, StructuredError> {
  complete_structured::<Invoice, Env, _>(ChatRequest::user("m", "Extract the invoice"))
    .provide_layer(model.layer::<StructuredError>())
    .block_on()
    .into_result()
    .map_err(|c| c.fail_option().expect("typed"))
}

#[test]
fn structured_output_runs_refinements() {
  // The JSON is well-formed and the JSON Schema shape matches, but the refinement refuses it.
  let model = ScriptedModel::new()
    .reply(r#"{"number": "INV-1", "total_cents": -5}"#)
    .reply(r#"{"number": "INV-1", "total_cents": -5}"#);
  let err = ask(&model).unwrap_err();
  assert!(
    matches!(&err, StructuredError::Decode { message, attempts: 2 } if message.contains("total_cents must be positive")),
    "{err:?}"
  );

  // A valid reply in a code fence decodes; the request carried the JSON Schema instruction.
  let model =
    ScriptedModel::new().reply("```json\n{\"number\": \"INV-2\", \"total_cents\": 990}\n```");
  assert_eq!(
    ask(&model).expect("decoded"),
    Invoice {
      number: "INV-2".into(),
      total_cents: 990
    }
  );
  let sent = &model.requests()[0].messages[0];
  assert_eq!(sent.role, ChatRole::System);
  assert_eq!(sent.content, schema_instruction::<Invoice>());
  assert!(sent.content.contains("\"total_cents\""));
}

#[test]
fn structured_output_repair_retry_bounded() {
  // First reply is not JSON, the repair succeeds: two calls, the second names the issue.
  let model = ScriptedModel::new()
    .respond(ChatResponse::text("Sure! The invoice is INV-3.").with_usage(Usage::new(3, 4)))
    .respond(
      ChatResponse::text(r#"{"number": "INV-3", "total_cents": 12}"#).with_usage(Usage::new(5, 6)),
    );
  let out = complete_structured_with::<Invoice>(
    model.service(),
    ChatRequest::user("m", "Extract"),
    StructuredOptions::default(),
  )
  .block_on()
  .into_result()
  .expect("repaired");
  assert_eq!(out.value.number, "INV-3");
  assert_eq!(out.attempts, 2);
  assert_eq!(out.usage, Usage::new(8, 10));
  let repair = model.requests()[1].messages.clone();
  assert_eq!(repair[repair.len() - 2].role, ChatRole::Assistant);
  assert!(repair[repair.len() - 1].content.contains("not JSON"));

  // Every reply is wrong: exactly 1 + MAX_REPAIR_RETRIES calls, then a typed error.
  let model = ScriptedModel::new()
    .reply("nope")
    .reply(r#"{"number": 1}"#)
    .reply("never asked");
  let err = ask(&model).unwrap_err();
  assert!(
    matches!(&err, StructuredError::Decode { attempts, message } if *attempts == 1 + MAX_REPAIR_RETRIES && message.contains("number")),
    "{err:?}"
  );
  assert_eq!(model.requests().len(), 2);
  assert_eq!(model.remaining(), 1, "no third call");
  assert!(
    err
      .to_string()
      .starts_with("structured output did not decode after 2 attempt(s): ")
  );

  // Asking for more retries is capped at one; asking for none makes one call.
  assert_eq!(
    StructuredOptions::default()
      .with_repair_retries(5)
      .repair_retries(),
    MAX_REPAIR_RETRIES
  );
  let model = ScriptedModel::new().reply("nope").reply("unused");
  let err = complete_structured_with::<Invoice>(
    model.service(),
    ChatRequest::user("m", "x"),
    StructuredOptions::default().without_repair(),
  )
  .block_on()
  .into_result()
  .unwrap_err()
  .fail_option()
  .expect("typed");
  assert!(matches!(err, StructuredError::Decode { attempts: 1, .. }));
  assert_eq!(model.remaining(), 1);
}

#[test]
fn model_failures_pass_through_typed() {
  let model = ScriptedModel::new().fail(AiError::Timeout);
  assert_eq!(
    ask(&model).unwrap_err(),
    StructuredError::Model(AiError::Timeout)
  );
  assert_eq!(
    StructuredError::from(AiError::Timeout).to_string(),
    "request timed out"
  );
  assert_eq!(
    decode_reply::<Invoice>(r#"{"number":"A","total_cents":1}"#)
      .expect("ok")
      .total_cents,
    1
  );
}
