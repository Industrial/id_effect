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

  #[tokio::test]
  async fn hedged_returns_backup_when_primary_is_slow() {
    use id_effect::kernel::{Effect, box_future};
    let primary = Effect::new_async(|_| {
      box_future(async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(1u32)
      })
    });
    let out = run_async(
      hedged(
        primary,
        succeed::<u32, (), ()>(2u32),
        Duration::from_millis(5),
      ),
      (),
    )
    .await
    .expect("hedged");
    assert_eq!(out, 2);
  }

  #[tokio::test]
  async fn hedged_fast_primary_failure_wins_the_race() {
    use id_effect::kernel::fail;
    let out = run_async(
      hedged(
        fail::<u32, &str, ()>("primary down"),
        succeed::<u32, &str, ()>(2u32),
        Duration::from_secs(60),
      ),
      (),
    )
    .await;
    assert_eq!(out, Err("primary down"));
  }

  #[tokio::test]
  async fn hedged_backup_is_not_started_before_delay() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let started = Arc::new(AtomicBool::new(false));
    let backup = {
      let started = Arc::clone(&started);
      Effect::new(move |_r: &mut ()| {
        started.store(true, Ordering::SeqCst);
        Ok::<u32, ()>(2)
      })
    };
    let out = run_async(
      hedged(
        succeed::<u32, (), ()>(1u32),
        backup,
        Duration::from_secs(60),
      ),
      (),
    )
    .await
    .expect("hedged");
    assert_eq!(out, 1);
    assert!(!started.load(Ordering::SeqCst));
  }
}
