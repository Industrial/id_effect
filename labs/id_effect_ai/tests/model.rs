use id_effect::{HCons, HNil};
use id_effect_ai::{
  AiError, ChatMessage, ChatRequest, ChatRole, LanguageModel, LanguageModelService,
  MockLanguageModel, complete, provide_mock_language_model,
};
use id_effect_tokio::TokioRuntime;

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
  let req = ChatRequest {
    model: "mock".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hello".into(),
    }],
  };
  let program = complete::<HCons<LanguageModelService, HNil>, _>(req)
    .provide_layer(provide_mock_language_model::<AiError>());
  let resp = TokioRuntime::current()
    .expect("tokio")
    .run(program)
    .await
    .into_result()
    .expect("complete");
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
  let rt = TokioRuntime::current().expect("tokio");
  let stream = rt
    .run(model.complete_stream(req))
    .await
    .into_result()
    .expect("stream");
  let chunks = rt
    .run(stream.run_collect())
    .await
    .into_result()
    .expect("collect");
  assert!(!chunks.is_empty());
}
