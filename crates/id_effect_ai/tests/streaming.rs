use id_effect_ai::mock_chunk_stream;
use id_effect_tokio::TokioRuntime;

#[test]
fn mock_chunk_stream_splits_text() {
  let stream = mock_chunk_stream("hello-world", 5);
  let chunks = stream
    .run_collect()
    .block_on()
    .into_result()
    .expect("collect");
  let text: String = chunks.iter().map(|c| c.delta.clone()).collect();
  assert_eq!(text, "hello-world");
}

#[tokio::test]
async fn mock_chunk_stream_collect_async() {
  let stream = mock_chunk_stream("abcd", 2);
  let chunks = TokioRuntime::current()
    .expect("tokio")
    .run(stream.run_collect())
    .await
    .into_result()
    .expect("collect");
  assert!(!chunks.is_empty());
  assert!(!chunks[0].delta.is_empty() || chunks[0].done);
}
