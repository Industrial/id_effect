//! The bounded agent loop (ai.llm #8, ADR 0015 D9): scripted, and against both vendors' wire
//! formats on wiremock (no live vendor call, D8).
#![cfg(all(feature = "tools", feature = "test-util"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use id_effect::{Effect, HCons, HNil, Schema, sleep, succeed};
use id_effect_ai::agent::{ToolLoop, ToolLoopBudget, ToolLoopOutcome};
use id_effect_ai::testing::ScriptedModel;
use id_effect_ai::tools::{Tool, ToolError, ToolPolicy, ToolSet};
use id_effect_ai::{
  AiError, ChatMessage, ChatRequest, ChatResponse, ChatRole, LanguageModelService, ToolCall, Usage,
};
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Schema)]
struct City {
  name: String,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Weather {
  celsius: i64,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Ack {
  ok: bool,
}

fn weather(runs: Arc<AtomicUsize>) -> Tool<City, Weather> {
  Tool::new(
    "weather",
    "Current weather in a city",
    move |c: City| -> Effect<Weather, String> {
      runs.fetch_add(1, Ordering::SeqCst);
      succeed(Weather {
        celsius: i64::try_from(c.name.len()).unwrap_or(0),
      })
    },
  )
}

fn tools(runs: &Arc<AtomicUsize>) -> ToolSet {
  ToolSet::new()
    .with(weather(Arc::clone(runs)))
    .expect("weather")
    .with(
      Tool::new(
        "book_trip",
        "Books a trip",
        |_: City| -> Effect<Ack, String> { succeed(Ack { ok: true }) },
      )
      .writes(),
    )
    .expect("book")
}

fn run_scripted(lp: &ToolLoop, model: &ScriptedModel) -> Result<ToolLoopOutcome, AiError> {
  lp.run_with(model.service(), ChatRequest::user("m", "Weather in Ghent?"))
    .block_on()
    .into_result()
    .map_err(|c| c.fail_option().expect("typed"))
}

#[test]
fn tool_loop_with_scripted_model() {
  let runs = Arc::new(AtomicUsize::new(0));
  let model = ScriptedModel::new()
    .respond(
      ChatResponse::text("checking")
        .with_tool_calls(vec![
          ToolCall::new("c1", "weather", json!({ "name": "Ghent" })),
          ToolCall::new("c2", "rm_rf", json!({})),
          ToolCall::new("c3", "book_trip", json!({ "name": "Ghent" })),
          ToolCall::new("c4", "weather", json!({ "city": "Ghent" })),
        ])
        .with_usage(Usage::new(10, 2)),
    )
    .respond(ChatResponse::text("It is 5 °C in Ghent.").with_usage(Usage::new(20, 3)));
  let lp = ToolLoop::new(tools(&runs));
  let out = run_scripted(&lp, &model).expect("loop");

  assert_eq!(out.response.content, "It is 5 °C in Ghent.");
  assert_eq!((out.steps, out.tool_calls), (2, 4));
  assert_eq!(out.usage, Usage::new(30, 5));
  assert_eq!(
    runs.load(Ordering::SeqCst),
    1,
    "only the valid registered read call ran"
  );
  assert!(matches!(out.refusals[0], ToolError::Unregistered { .. }));
  assert!(matches!(out.refusals[1], ToolError::WriteDenied { .. }));
  assert!(matches!(
    out.refusals[2],
    ToolError::InvalidArguments { .. }
  ));

  // The first request offered exactly the set's tools; the second carried the transcript.
  let requests = model.requests();
  let offered: Vec<_> = requests[0].tools.iter().map(|t| t.name.as_str()).collect();
  assert_eq!(offered, ["book_trip", "weather"]);
  let sent = &requests[1].messages;
  assert_eq!(sent.len(), 6);
  assert_eq!(sent[1].role, ChatRole::Assistant);
  assert_eq!(sent[1].tool_calls.len(), 4);
  assert_eq!(sent[2], ChatMessage::tool_result("c1", r#"{"celsius":5}"#));
  assert_eq!(sent[3].tool_call_id.as_deref(), Some("c2"));
  assert!(sent[3].content.contains("not available"));
  assert!(sent[4].content.contains("not allowed"));
  assert!(sent[5].content.contains("invalid arguments"));
  assert_eq!(&out.messages, sent);

  // With writes allowed, the write tool runs.
  let model = ScriptedModel::new()
    .tool_call("c1", "book_trip", json!({ "name": "Ghent" }))
    .reply("booked");
  let out = run_scripted(
    &ToolLoop::new(tools(&runs)).with_policy(ToolPolicy::default().allow_writes()),
    &model,
  )
  .expect("loop");
  assert!(out.refusals.is_empty());
  assert_eq!(out.messages[2].content, r#"{"ok":true}"#);
}

#[test]
fn ai_tool_loop_step_budget_enforced() {
  let runs = Arc::new(AtomicUsize::new(0));
  let model = ScriptedModel::new();
  for i in 0..10 {
    model.push(id_effect_ai::testing::ScriptedReply::Reply(
      ChatResponse::text("").with_tool_calls(vec![ToolCall::new(
        format!("c{i}"),
        "weather",
        json!({ "name": "x" }),
      )]),
    ));
  }
  let lp = ToolLoop::new(tools(&runs)).with_budget(ToolLoopBudget::default().with_max_steps(3));
  let err = run_scripted(&lp, &model).unwrap_err();
  assert_eq!(
    err,
    AiError::LimitExceeded {
      limit: "tool_loop_steps",
      max: 3
    }
  );
  assert_eq!(model.requests().len(), 3, "three model calls");
  assert_eq!(
    runs.load(Ordering::SeqCst),
    2,
    "the last step's calls did not run"
  );

  // The default is 8 steps: a model that never stops is cut off there.
  let model = ScriptedModel::new();
  for i in 0..20 {
    model.push(id_effect_ai::testing::ScriptedReply::Reply(
      ChatResponse::text("").with_tool_calls(vec![ToolCall::new(
        format!("c{i}"),
        "weather",
        json!({ "name": "x" }),
      )]),
    ));
  }
  let err = run_scripted(&ToolLoop::new(tools(&runs)), &model).unwrap_err();
  assert_eq!(
    err,
    AiError::LimitExceeded {
      limit: "tool_loop_steps",
      max: 8
    }
  );
  assert_eq!(model.requests().len(), 8);
}

#[test]
fn tool_call_budget_enforced_before_any_call_runs() {
  let runs = Arc::new(AtomicUsize::new(0));
  let calls: Vec<_> = (0..3)
    .map(|i| ToolCall::new(format!("c{i}"), "weather", json!({ "name": "x" })))
    .collect();
  let model = ScriptedModel::new()
    .respond(ChatResponse::text("").with_tool_calls(calls.clone()))
    .respond(ChatResponse::text("").with_tool_calls(calls));
  let lp =
    ToolLoop::new(tools(&runs)).with_budget(ToolLoopBudget::default().with_max_tool_calls(4));
  let err = run_scripted(&lp, &model).unwrap_err();
  assert_eq!(
    err,
    AiError::LimitExceeded {
      limit: "tool_loop_tool_calls",
      max: 4
    }
  );
  assert_eq!(
    runs.load(Ordering::SeqCst),
    3,
    "the second batch never started"
  );
}

#[test]
fn tool_output_cap_and_duration_budget() {
  // An output over the cap is refused and fed back; the loop goes on.
  let runs = Arc::new(AtomicUsize::new(0));
  let model = ScriptedModel::new()
    .tool_call("c1", "weather", json!({ "name": "Ghent" }))
    .reply("done");
  let lp = ToolLoop::new(tools(&runs))
    .with_budget(ToolLoopBudget::default().with_max_tool_output_bytes(4));
  let out = run_scripted(&lp, &model).expect("loop");
  assert_eq!(
    out.refusals,
    vec![ToolError::OutputTooLarge {
      tool: "weather".into(),
      max: 4
    }]
  );

  // A tool that outlives the run's time budget is interrupted.
  let slow = Tool::new("slow", "Sleeps", |_: City| -> Effect<Ack, String> {
    sleep(Duration::from_secs(30)).map(|()| Ack { ok: true })
  });
  let model = ScriptedModel::new()
    .tool_call("c1", "slow", json!({ "name": "x" }))
    .reply("unreachable");
  let lp = ToolLoop::new(ToolSet::new().with(slow).expect("slow"))
    .with_budget(ToolLoopBudget::default().with_max_duration(Duration::from_millis(50)));
  let err = run_scripted(&lp, &model).unwrap_err();
  assert_eq!(
    err,
    AiError::LimitExceeded {
      limit: "tool_loop_duration_ms",
      max: 50
    }
  );
}

#[test]
fn loop_reads_the_model_from_the_environment_and_passes_model_errors() {
  let runs = Arc::new(AtomicUsize::new(0));
  let model = ScriptedModel::new()
    .reply("no tools needed")
    .fail(AiError::Unauthorized);
  let lp = ToolLoop::new(tools(&runs));
  let run = |lp: &ToolLoop| {
    lp.run::<HCons<LanguageModelService, HNil>, _>(ChatRequest::user("m", "hi"))
      .provide_layer(model.layer::<AiError>())
      .block_on()
      .into_result()
      .map_err(|c| c.fail_option().expect("typed"))
  };
  let out = run(&lp).expect("loop");
  assert_eq!((out.steps, out.tool_calls), (1, 0));
  assert_eq!(run(&lp).unwrap_err(), AiError::Unauthorized);
}

#[cfg(all(feature = "openai", feature = "anthropic"))]
mod vendors {
  use super::*;

  use id_effect::Exit;
  use id_effect_ai::{AiConfig, AnthropicLanguageModel, OpenAiLanguageModel};
  use id_effect_platform::http::HttpClientConfig;
  use id_effect_tokio::TokioRuntime;
  use wiremock::matchers::{method, path};
  use wiremock::{Mock, MockServer, ResponseTemplate};

  async fn run<A: Send + 'static>(eff: Effect<A, AiError>) -> Exit<A, AiError> {
    TokioRuntime::current().expect("tokio").run(eff).await
  }

  fn config(uri: &str) -> AiConfig {
    AiConfig::default()
      .with_openai_api_key("k")
      .with_openai_base_url(uri)
      .with_anthropic_api_key("k")
      .with_anthropic_base_url(uri)
      .with_http(HttpClientConfig::default().allow_hosts(["127.0.0.1"]))
  }

  async fn bodies(server: &MockServer) -> Vec<serde_json::Value> {
    server
      .received_requests()
      .await
      .expect("recorded")
      .iter()
      .map(|r| serde_json::from_slice(&r.body).expect("json body"))
      .collect()
  }

  #[tokio::test]
  async fn tool_loop_round_trip_openai_and_anthropic() {
    // OpenAI Chat Completions: `tools` of type function, `tool_calls` with JSON-text arguments,
    // results as `role: tool`.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
      .and(path("/v1/chat/completions"))
      .respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"content": null, "tool_calls": [{
          "id": "call_1", "type": "function",
          "function": {"name": "weather", "arguments": "{\"name\":\"Ghent\"}"}
        }]}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 7, "completion_tokens": 3}
      })))
      .up_to_n_times(1)
      .mount(&server)
      .await;
    Mock::given(method("POST"))
      .and(path("/v1/chat/completions"))
      .respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"content": "5 °C"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 9, "completion_tokens": 2}
      })))
      .mount(&server)
      .await;
    let runs = Arc::new(AtomicUsize::new(0));
    let model: LanguageModelService =
      Arc::new(OpenAiLanguageModel::from_config(&config(&server.uri())).expect("openai"));
    let out =
      run(ToolLoop::new(tools(&runs)).run_with(model, ChatRequest::user("gpt", "weather?")))
        .await
        .into_result()
        .expect("loop");
    assert_eq!(out.response.content, "5 °C");
    assert_eq!(out.usage, Usage::new(16, 5));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["tools"][1]["type"], "function");
    assert_eq!(sent[0]["tools"][1]["function"]["name"], "weather");
    assert_eq!(
      sent[0]["tools"][1]["function"]["parameters"]["properties"]["name"]["type"],
      "string"
    );
    let msgs = sent[1]["messages"].as_array().expect("messages");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
      msgs[1]["tool_calls"][0]["function"]["arguments"],
      "{\"name\":\"Ghent\"}"
    );
    assert_eq!(
      msgs[2],
      json!({"role": "tool", "tool_call_id": "call_1", "content": "{\"celsius\":5}"})
    );

    // Anthropic Messages: `tools` with `input_schema`, `tool_use` blocks, results as
    // `tool_result` blocks in the next user turn.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "content": [
          {"type": "text", "text": "Let me check."},
          {"type": "tool_use", "id": "toolu_1", "name": "weather", "input": {"name": "Ghent"}}
        ],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 4, "output_tokens": 6}
      })))
      .up_to_n_times(1)
      .mount(&server)
      .await;
    Mock::given(method("POST"))
      .and(path("/v1/messages"))
      .respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "content": [{"type": "text", "text": "5 °C in Ghent"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 8, "output_tokens": 2}
      })))
      .mount(&server)
      .await;
    let model: LanguageModelService =
      Arc::new(AnthropicLanguageModel::from_config(&config(&server.uri())).expect("anthropic"));
    let out =
      run(ToolLoop::new(tools(&runs)).run_with(model, ChatRequest::user("claude", "weather?")))
        .await
        .into_result()
        .expect("loop");
    assert_eq!(out.response.content, "5 °C in Ghent");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["tools"][1]["name"], "weather");
    assert_eq!(sent[0]["tools"][1]["input_schema"]["type"], "object");
    let msgs = sent[1]["messages"].as_array().expect("messages");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(
      msgs[1]["content"][0],
      json!({"type": "text", "text": "Let me check."})
    );
    assert_eq!(
      msgs[1]["content"][1],
      json!({"type": "tool_use", "id": "toolu_1", "name": "weather", "input": {"name": "Ghent"}})
    );
    assert_eq!(
      msgs[2],
      json!({"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "{\"celsius\":5}"}
      ]})
    );
  }
}
