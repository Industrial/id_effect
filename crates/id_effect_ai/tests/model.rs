//! The `LanguageModel` capability with the echo mock.

use id_effect::{HCons, HNil};
use id_effect_ai::{
  AiError, ChatMessage, ChatRequest, LanguageModel, LanguageModelService, MockLanguageModel,
  complete, provide_mock_language_model,
};
use id_effect_tokio::TokioRuntime;

#[test]
fn chat_request_validate_empty_fails() {
  let req = ChatRequest::new("mock", Vec::<ChatMessage>::new());
  assert!(req.validate().is_err());
}

#[test]
fn mock_complete_echoes_user() {
  let resp = MockLanguageModel::echo()
    .complete(ChatRequest::user("mock", "hello"))
    .block_on()
    .into_result()
    .expect("run");
  assert_eq!(resp.content, "echo:hello");
  assert_eq!(resp.usage.output_tokens, 10);
}

#[tokio::test]
async fn mock_complete_echoes_user_message() {
  let program =
    complete::<HCons<LanguageModelService, HNil>, _>(ChatRequest::user("mock", "hello"))
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
  let rt = TokioRuntime::current().expect("tokio");
  let stream = rt
    .run(MockLanguageModel::echo().complete_stream(ChatRequest::user("mock", "ab")))
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
