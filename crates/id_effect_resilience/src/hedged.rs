//! Hedged requests — race a delayed backup against a primary effect.

use std::time::Duration;

use id_effect::kernel::{Effect, box_future};
use id_effect::runtime::run_async;

/// Run `primary` and, after `delay`, `backup`; first completion wins.
///
/// Intended for effects with environment `()` (each branch runs with a fresh `()` env).
pub fn hedged<A, E>(
  primary: Effect<A, E, ()>,
  backup: Effect<A, E, ()>,
  delay: Duration,
) -> Effect<A, E, ()>
where
  A: Send + 'static,
  E: Send + Sync + 'static,
{
  Effect::new_async(move |_r: &mut ()| {
    box_future(async move {
      tokio::select! {
        out = run_async(primary, ()) => out,
        out = async {
          tokio::time::sleep(delay).await;
          run_async(backup, ()).await
        } => out,
      }
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::kernel::succeed;
  use id_effect::runtime::run_async;

  #[tokio::test]
  async fn hedged_returns_primary_when_fast() {
    let out = run_async(
      hedged(
        succeed::<u32, (), ()>(1u32),
        succeed::<u32, (), ()>(2u32),
        Duration::from_millis(100),
      ),
      (),
    )
    .await
    .expect("hedged");
    assert_eq!(out, 1);
  }
}
