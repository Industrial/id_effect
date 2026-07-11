//! Integration test: auto-parallel `effect!` binds run concurrently.

use std::time::{Duration, Instant};

use id_effect::{Effect, effect, from_async, run_async};

fn delay_ms(ms: u64) -> Effect<(), (), ()> {
  from_async(move |_: &mut ()| async move {
    tokio::time::sleep(Duration::from_millis(ms)).await;
    Ok(())
  })
}

#[tokio::test]
async fn parallel_binds_complete_under_sequential_budget() {
  let start = Instant::now();
  let result: Result<(), ()> = run_async(
    effect! {
      let _a = ~ delay_ms(40);
      let _b = ~ delay_ms(40);
      ()
    },
    (),
  )
  .await;
  let elapsed = start.elapsed();
  assert!(result.is_ok());
  assert!(
    elapsed < Duration::from_millis(70),
    "expected parallel binds (<70ms), got {elapsed:?}"
  );
}

#[tokio::test]
async fn three_parallel_binds_complete_under_sequential_budget() {
  let start = Instant::now();
  let result: Result<(), ()> = run_async(
    effect! {
      let _a = ~ delay_ms(40);
      let _b = ~ delay_ms(40);
      let _c = ~ delay_ms(40);
      ()
    },
    (),
  )
  .await;
  let elapsed = start.elapsed();
  assert!(result.is_ok());
  assert!(
    elapsed < Duration::from_millis(70),
    "expected three parallel binds (<70ms), got {elapsed:?}"
  );
}
