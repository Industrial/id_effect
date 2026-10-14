//! Stream test helpers — bounded-memory assertions for pull-based [`Stream`] pipelines.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Effect;
use crate::runtime::run_blocking;
use crate::streaming::stream::Stream;

/// Run `pipeline` over an instrumented **infinite** source (`0, 1, 2, …`), take `n` outputs, and
/// assert that the pipeline never buffers more than `max_in_flight` source items.
///
/// "In flight" is a proxy for memory: the number of items the source has produced since the
/// consumer last received a chunk (produced-but-not-yet-delivered). A pull-based pipeline keeps
/// this around one chunk (64 items by default, fewer under `take`), whereas an operator that
/// drains its upstream before emitting would run away — the source panics as soon as the bound
/// is exceeded, so an eager operator fails the test instead of hanging. Choose `max_in_flight` of
/// at least the chunk size, plus any gap a selective `filter` may skip between outputs.
///
/// Returns the `n` outputs (fewer if the pipeline ends early).
///
/// # Panics
///
/// If the bound is exceeded, or if the pipeline fails.
///
/// ```
/// use id_effect::testing::assert_bounded_memory;
///
/// let out = assert_bounded_memory(|s| s.map(|n| n * 2), 5, 64);
/// assert_eq!(out, vec![0, 2, 4, 6, 8]);
/// ```
pub fn assert_bounded_memory<B, F>(pipeline: F, n: usize, max_in_flight: u64) -> Vec<B>
where
  B: Send + 'static,
  F: FnOnce(Stream<u64, (), ()>) -> Stream<B, (), ()>,
{
  let produced = Arc::new(AtomicU64::new(0));
  let delivered = Arc::new(AtomicU64::new(0));

  let source = {
    let produced = Arc::clone(&produced);
    let delivered = Arc::clone(&delivered);
    Stream::from_pull(move |_env: &mut (), want| {
      let start = produced.fetch_add(want as u64, Ordering::SeqCst);
      let end = start + want as u64;
      let in_flight = end - delivered.load(Ordering::SeqCst);
      assert!(
        in_flight <= max_in_flight,
        "assert_bounded_memory: {in_flight} source items produced but not yet delivered \
         (bound {max_in_flight}); the pipeline is buffering its upstream"
      );
      let batch: Vec<u64> = (start..end).collect();
      Box::pin(async move { Ok(Some(batch)) })
    })
  };

  let mut stream = pipeline(source).take(n);
  let consume = Effect::new_async(move |r: &mut ()| {
    Box::pin(async move {
      let mut out = Vec::new();
      while let Some(chunk) = stream.poll_next_chunk(r).await? {
        delivered.store(produced.load(Ordering::SeqCst), Ordering::SeqCst);
        out.extend(chunk.into_vec());
      }
      Ok::<_, ()>(out)
    })
  });
  run_blocking(consume, ()).expect("assert_bounded_memory: pipeline failed")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn identity_pipeline_returns_first_n_naturals() {
    assert_eq!(assert_bounded_memory(|s| s, 3, 64), vec![0, 1, 2]);
  }

  #[test]
  #[should_panic(expected = "buffering its upstream")]
  fn eager_pipeline_exceeds_bound() {
    // `run_collect` on an infinite source drains forever; the source panics at the bound.
    let _ = assert_bounded_memory(|s| Stream::from_effect(s.run_collect()).map(|v| v), 1, 256);
  }
}
