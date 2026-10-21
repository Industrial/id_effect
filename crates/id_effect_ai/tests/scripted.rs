//! `testing::ScriptedModel`: deterministic replies, tool calls and streams (D8).
#![cfg(feature = "test-util")]

use id_effect::{HCons, HNil};
use id_effect_ai::testing::{ScriptedModel, ScriptedReply};
use id_effect_ai::{
  AiError, ChatRequest, ChatResponse, LanguageModelService, Usage, complete, complete_stream,
};
use id_effect_tokio::TokioRuntime;

type Env = HCons<LanguageModelService, HNil>;

#[tokio::test]
async fn scripted_model_replays_in_order() {
  let model = ScriptedModel::new()
    .reply("one")
    .tool_call("call-1", "lookup", serde_json::json!({"sku": "A-1"}))
    .respond(
      ChatResponse::text("three")
        .with_usage(Usage::new(1, 2))
        .with_stop_reason("stop"),
    )
    .fail(AiError::Transient {
      status: 429,
      detail: "rate".into(),
    });
  model.push(ScriptedReply::Stream(vec!["fi".into(), "ve".into()]));
  let rt = TokioRuntime::current().expect("tokio");
  let ask = |text: &str| {
    complete::<Env, _>(ChatRequest::user("m", text)).provide_layer(model.layer::<AiError>())
  };
  let one = rt.run(ask("q1")).await.into_result().expect("one");
  assert_eq!(one.content, "one");
  let two = rt.run(ask("q2")).await.into_result().expect("two");
  assert_eq!(two.tool_calls[0].name, "lookup");
  assert_eq!(two.tool_calls[0].arguments["sku"], "A-1");
  let three = rt.run(ask("q3")).await.into_result().expect("three");
  assert_eq!(three.usage.total(), 3);
  let four = rt.run(ask("q4")).await.into_result().unwrap_err();
  assert!(four.fail_option().expect("typed").is_transient());
  let five = rt
    .run(
      complete_stream::<Env, _>(ChatRequest::user("m", "q5"))
        .provide_layer(model.layer::<AiError>()),
    )
    .await
    .into_result()
    .expect("stream");
  let chunks = rt
    .run(five.run_collect())
    .await
    .into_result()
    .expect("chunks");
  let text: String = chunks.iter().map(|c| c.delta.as_str()).collect();
  assert_eq!(text, "five");

  // The requests were recorded in order, and the script is used up.
  let asked: Vec<String> = model
    .requests()
    .iter()
    .map(|r| r.messages[0].content.clone())
    .collect();
  assert_eq!(asked, ["q1", "q2", "q3", "q4", "q5"]);
  assert_eq!(model.remaining(), 0);
  let exhausted = rt.run(ask("q6")).await.into_result().unwrap_err();
  assert!(matches!(exhausted.fail_option(), Some(AiError::Upstream(m)) if m.contains("call 6")));
}
