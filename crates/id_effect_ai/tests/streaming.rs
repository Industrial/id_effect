use id_effect::{run_async, run_blocking};
use id_effect_ai::mock_chunk_stream;

#[test]
fn mock_chunk_stream_splits_text() {
  let stream = mock_chunk_stream("hello-world", 5);
  let chunks = run_blocking(stream.run_collect(), ()).expect("collect");
  let text: String = chunks.iter().map(|c| c.delta.clone()).collect();
  assert_eq!(text, "hello-world");
}

#[tokio::test]
async fn mock_chunk_stream_collect_async() {
  let stream = mock_chunk_stream("abcd", 2);
  let chunks = run_async(stream.run_collect(), ()).await.expect("collect");
  assert!(!chunks.is_empty());
  assert!(!chunks[0].delta.is_empty() || chunks[0].done);
}
