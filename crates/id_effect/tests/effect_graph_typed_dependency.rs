//! EDG: type-annotated binds register dependencies (regression for false parallelization).

use id_effect::{effect, kernel::succeed, run_async};

#[tokio::test]
async fn typed_bind_dependent_step_runs() {
  let result: Result<u32, ()> = run_async(
    effect! {
      let filtered: Vec<u32> = ~ succeed(vec![1u32, 2, 3]);
      let kept = ~ succeed(filtered.len() as u32);
      kept
    },
    (),
  )
  .await;
  assert_eq!(result, Ok(3));
}
