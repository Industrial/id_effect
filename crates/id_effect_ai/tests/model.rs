use id_effect::{build_env, run_async, run_blocking};
use id_effect_ai::{
  ChatMessage, ChatRequest, ChatRole, LanguageModel, MockLanguageModel, complete,
  provide_mock_language_model,
};

#[test]
fn chat_request_validate_empty_fails() {
  let req = ChatRequest {
    model: "mock".into(),
    messages: vec![],
  };
  assert!(req.validate().is_err());
}

#[tokio::test]
async fn mock_complete_echoes_user_message() {
  let env = build_env([provide_mock_language_model()]).expect("env");
  let req = ChatRequest {
    model: "mock".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hello".into(),
    }],
  };
  let resp = run_async(complete(req), env).await.expect("complete");
  assert!(resp.content.contains("hello"));
}

#[tokio::test]
async fn mock_stream_yields_chunks() {
  let model = MockLanguageModel::echo();
  let req = ChatRequest {
    model: "mock".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "ab".into(),
    }],
  };
  let stream = run_blocking(model.complete_stream(req), ()).expect("stream");
  let chunks = run_async(stream.run_collect(), ()).await.expect("collect");
  assert!(!chunks.is_empty());
}
