//! Streaming completions as `id_effect::Stream`.

use id_effect::{HCons, HNil};
use id_effect_ai::{
  AiError, ChatRequest, LanguageModelService, complete_stream, mock_chunk_stream,
  provide_mock_language_model,
};
use id_effect_tokio::TokioRuntime;

#[test]
fn mock_chunk_stream_splits_text() {
  let chunks = mock_chunk_stream("hello-world", 5)
    .run_collect()
    .block_on()
    .into_result()
    .expect("collect");
  let text: String = chunks.iter().map(|c| c.delta.clone()).collect();
  assert_eq!(text, "hello-world");
}

#[tokio::test]
async fn mock_chunk_stream_collect_async() {
  let chunks = TokioRuntime::current()
    .expect("tokio")
    .run(mock_chunk_stream("abcd", 2).run_collect())
    .await
    .into_result()
    .expect("collect");
  assert!(!chunks.is_empty());
  assert!(!chunks[0].delta.is_empty() || chunks[0].done);
}

#[tokio::test]
async fn complete_stream_reads_the_capability() {
  let rt = TokioRuntime::current().expect("tokio");
  let stream = rt
    .run(
      complete_stream::<HCons<LanguageModelService, HNil>, _>(ChatRequest::user("m", "xyz"))
        .provide_layer(provide_mock_language_model::<AiError>()),
    )
    .await
    .into_result()
    .expect("stream");
  let chunks = rt
    .run(stream.run_collect())
    .await
    .into_result()
    .expect("collect");
  let text: String = chunks.iter().map(|c| c.delta.as_str()).collect();
  assert_eq!(text, "echo:xyz");
  assert!(chunks.last().expect("last").done);
}
