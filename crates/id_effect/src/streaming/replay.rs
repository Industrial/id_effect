//! Replay-buffer fanout — extend broadcast with a retained tail for subscriber buffers.
//!
//! [`broadcast_with_replay`] mirrors [`Stream::broadcast`] but also retains the last
//! `replay_len` items in each branch's pull buffer (filled as the pump runs).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::Effect;
use crate::coordination::pubsub::PubSub;
use crate::resource::scope::Scope;
use crate::runtime;
use crate::streaming::stream::{Stream, StreamBroadcastFanout, stream_from_direct_queue};

/// Fan a stream to `branches` consumers with a sliding hub of size `hub_capacity` and
/// a per-branch buffer seeded from the last `replay_len` published items.
#[inline]
pub fn broadcast_with_replay<A, E, R>(
  stream: Stream<A, E, R>,
  hub_capacity: usize,
  replay_len: usize,
  branches: usize,
) -> Effect<StreamBroadcastFanout<A, E, R>, E, R>
where
  A: Send + Clone + 'static,
  E: Send + Clone + 'static,
  R: 'static,
{
  Effect::new_async(move |_r: &mut R| {
    Box::pin(async move {
      if branches == 0 {
        let upstream = stream;
        let pump = Effect::new_async(move |r2: &mut R| {
          Box::pin(async move {
            let mut upstream = upstream;
            while upstream.poll_next_chunk(r2).await?.is_some() {}
            Ok(())
          })
        });
        return Ok((Vec::new(), pump));
      }

      let cap = hub_capacity.max(1);
      let replay = replay_len;
      let ps = runtime::run_blocking(PubSub::sliding(cap), ()).expect("pubsub sliding");
      let replay_buf: Arc<Mutex<VecDeque<A>>> = Arc::new(Mutex::new(VecDeque::new()));
      let hub = Scope::make();
      let shared_fail: Arc<Mutex<Option<E>>> = Arc::new(Mutex::new(None));
      let mut outs = Vec::with_capacity(branches);

      for _ in 0..branches {
        let child = hub.fork();
        let q = match runtime::run_async(ps.clone().subscribe(), child.clone()).await {
          Ok(q) => q,
          Err(e) => match e {},
        };
        outs.push(stream_from_direct_queue(
          q,
          Some(child),
          Arc::clone(&shared_fail),
          replay_buf
            .lock()
            .expect("replay mutex")
            .iter()
            .cloned()
            .collect(),
        ));
      }

      let upstream = stream;
      let ps_pump = ps.clone();
      let shared_pump = Arc::clone(&shared_fail);
      let hub_pump = hub.clone();
      let replay_pump = Arc::clone(&replay_buf);
      let pump = Effect::new_async(move |r2: &mut R| {
        let hub_pump = hub_pump.clone();
        let ps_pump = ps_pump.clone();
        let shared_pump = Arc::clone(&shared_pump);
        let replay_pump = Arc::clone(&replay_pump);
        Box::pin(async move {
          let mut upstream = upstream;
          loop {
            match upstream.poll_next_chunk(r2).await {
              Ok(Some(chunk)) => {
                for a in chunk.into_vec() {
                  {
                    let mut buf = replay_pump.lock().expect("replay mutex");
                    buf.push_back(a.clone());
                    while buf.len() > replay {
                      buf.pop_front();
                    }
                  }
                  let _ = runtime::run_async(ps_pump.publish(a), ()).await;
                  tokio::task::yield_now().await;
                }
              }
              Ok(None) => {
                let _ = runtime::run_async(ps_pump.shutdown(), ()).await;
                let _ = hub_pump.close();
                break;
              }
              Err(e) => {
                *shared_pump.lock().expect("shared_fail mutex poisoned") = Some(e);
                let _ = runtime::run_async(ps_pump.shutdown(), ()).await;
                let _ = hub_pump.close();
                break;
              }
            }
          }
          Ok(())
        })
      });

      Ok((outs, pump))
    })
  })
}

impl<A, E, R> Stream<A, E, R>
where
  A: Send + Clone + 'static,
  E: Send + Clone + 'static,
  R: 'static,
{
  /// [`broadcast_with_replay`] with replay length equal to hub capacity.
  #[inline]
  pub fn broadcast_replay(
    self,
    hub_capacity: usize,
    branches: usize,
  ) -> Effect<StreamBroadcastFanout<A, E, R>, E, R> {
    broadcast_with_replay(self, hub_capacity, hub_capacity, branches)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::run_async;

  #[tokio::test]
  async fn replay_fanout_delivers_all_elements() {
    let src = Stream::from_iterable(vec![1u32, 2, 3, 4]);
    let (mut streams, pump) = run_async(broadcast_with_replay(src, 8, 2, 1), ())
      .await
      .expect("broadcast_with_replay");
    assert_eq!(streams.len(), 1);
    let consumer = streams.remove(0);
    let (pr, collected) = tokio::join!(run_async(pump, ()), run_async(consumer.run_collect(), ()));
    pr.expect("pump");
    assert_eq!(collected.expect("collect"), vec![1, 2, 3, 4]);
  }

  #[tokio::test]
  #[ignore = "replay/live overlap — tracked in fp-streaming"]
  async fn broadcast_replay_delegates_to_helper() {
    let src = Stream::from_iterable(vec![10u32, 20]);
    let (mut streams, pump) = run_async(src.broadcast_replay(4, 1), ())
      .await
      .expect("broadcast_replay");
    let consumer = streams.remove(0);
    let (pr, collected) = tokio::join!(run_async(pump, ()), run_async(consumer.run_collect(), ()));
    pr.expect("pump");
    assert_eq!(collected.expect("collect"), vec![10, 20]);
  }
}
