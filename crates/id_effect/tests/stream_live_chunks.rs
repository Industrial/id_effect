//! 0.13 F57 (`r13-f-stream-chunking`): a pull on a live stream hands over the elements that are
//! already there instead of waiting for a full chunk.
//!
//! [`Stream::unfold_effect`] waits for the first element of a pull only; it then keeps the steps
//! that complete without waiting and stops at the first one that would wait (a timer, an empty
//! queue, a pending future), which the next pull resumes. So `run_for_each` on a live client
//! stream sees each element as it arrives, while bounded and pure sources keep full chunks.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::{
  Cause, Chunk, DEFAULT_CHUNK_SIZE, Effect, Exit, FiberHandle, HNil, Never, Queue, Stream,
  TestExecutor, die, fail, from_async, sleep, succeed, suspend, sync, yield_fiber,
};

// ── helpers ─────────────────────────────────────────────────────────────────────────────────

fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("effect died: {c:?}"),
  }
}

fn ok<A: Send + 'static>(eff: Effect<A, Never>) -> A {
  match run(eff) {
    Ok(a) => a,
    Err(n) => match n {},
  }
}

/// One pulled chunk (as a vector) and the rest of the stream.
type Pulled<A, E> = (Vec<A>, Stream<A, E>);

/// One pull of at most `max` elements: the chunk as a vector and the rest, or `None` at the end.
fn pull<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(
  s: Stream<A, E>,
  max: usize,
) -> Result<Option<Pulled<A, E>>, E> {
  run(s.next_chunk_with_size(max)).map(|o| o.map(|(c, rest)| (c.into_vec(), rest)))
}

/// Every chunk of `s`, pulling `max` at a time.
fn chunks<A: Send + 'static>(mut s: Stream<A>, max: usize) -> Vec<Vec<A>> {
  let mut out = Vec::new();
  while let Some((chunk, rest)) = ok(s.next_chunk_with_size(max)) {
    out.push(Chunk::into_vec(chunk));
    s = rest;
  }
  out
}

fn sizes<A>(chunks: &[Vec<A>]) -> Vec<usize> {
  chunks.iter().map(Vec::len).collect()
}

/// The sizes a source of `len` elements must keep when every element is ready at once.
fn full_chunks(len: usize, max: usize) -> Vec<usize> {
  let mut out = vec![max; len / max];
  if !len.is_multiple_of(max) {
    out.push(len % max);
  }
  out
}

/// `Pending` once (after waking its waker), then ready: a step that would wait.
struct PendingOnce(bool);

impl Future for PendingOnce {
  type Output = ();
  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    if self.0 {
      Poll::Ready(())
    } else {
      self.0 = true;
      cx.waker().wake_by_ref();
      Poll::Pending
    }
  }
}

/// The buffered frames and the waker of a `next` waiting for one.
type FrameState = (VecDeque<Option<u32>>, Option<Waker>);

/// A frame source shaped like an RPC client stream's transport (`id_effect_rpc` decodes each
/// frame in a `from_async` step): a frame is `Some(item)` or the end (`None`); `next` waits
/// while no frame is buffered.
#[derive(Clone, Default)]
struct Frames {
  inner: Arc<Mutex<FrameState>>,
}

impl Frames {
  fn push(&self, frame: Option<u32>) {
    let waker = {
      let mut g = self.inner.lock().unwrap();
      g.0.push_back(frame);
      g.1.take()
    };
    if let Some(w) = waker {
      w.wake();
    }
  }

  fn next(&self) -> NextFrame {
    NextFrame(self.clone())
  }
}

struct NextFrame(Frames);

impl Future for NextFrame {
  type Output = Option<u32>;
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<u32>> {
    let mut g = self.0.inner.lock().unwrap();
    match g.0.pop_front() {
      Some(frame) => Poll::Ready(frame),
      None => {
        g.1 = Some(cx.waker().clone());
        Poll::Pending
      }
    }
  }
}

/// The client-stream shape: one `from_async` step per frame, decoded by a `map`.
fn client_stream(frames: Frames) -> Stream<u32> {
  Stream::unfold_effect(frames, |frames| {
    let next = frames.next();
    from_async(move || async move { Ok::<_, Never>(next.await) })
      .map(move |frame| frame.map(|item| (item * 10, frames)))
  })
}

/// Runs `s` collecting into a shared vector, so a test can watch the output as it arrives.
fn watch(s: Stream<u32>) -> (Arc<Mutex<Vec<u32>>>, Effect<(), Never>) {
  let seen = Arc::new(Mutex::new(Vec::new()));
  let sink = Arc::clone(&seen);
  let eff = s.run_for_each(move |x| {
    let sink = Arc::clone(&sink);
    sync(move || sink.lock().unwrap().push(x))
  });
  (seen, eff)
}

/// Forks `eff` on a fresh `TestExecutor` and runs it until it stalls.
fn spawn_test<A: Send + 'static, E: Send + 'static>(
  eff: Effect<A, E>,
) -> (TestExecutor, FiberHandle<A, E>) {
  let exec = TestExecutor::new();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  (exec, h)
}

fn seen_now(seen: &Arc<Mutex<Vec<u32>>>) -> Vec<u32> {
  seen.lock().unwrap().clone()
}

fn joined<A: Send + 'static, E: Send + 'static + std::fmt::Debug>(h: FiberHandle<A, E>) -> A {
  assert!(h.is_done(), "fiber still running");
  let mut fut = pin!(h.join_future());
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(Exit::Success(a)) => a,
    Poll::Ready(Exit::Failure(c)) => panic!("fiber failed: {c:?}"),
    Poll::Pending => panic!("join is not ready"),
  }
}

/// The queue's elements until it is shut down and drained, one `take` per step.
fn from_takes(q: &Queue<u32>) -> Stream<u32> {
  let q = q.clone();
  Stream::unfold_effect((), move |()| {
    q.take::<HNil>()
      .map(|n| Some((n, ())))
      .catch(|_| succeed(None))
  })
}

fn offer(q: &Queue<u32>, items: impl IntoIterator<Item = u32>) {
  let left = ok(q.try_offer_all::<Never, HNil>(items));
  assert!(left.is_empty());
}

// ── the leaf's acceptance tests ────────────────────────────────────────────────────────────

/// A pull on a live source hands over what is there (between 1 and the requested size) instead
/// of waiting for a full chunk, and the next pull resumes the step that would have waited.
#[test]
fn live_stream_delivers_partial_chunk() {
  let q = Queue::<u32>::unbounded();
  offer(&q, [1, 2, 3]);
  let s = from_takes(&q);

  let (first, s) = ok(s.next_chunk()).expect("a chunk");
  assert_eq!(
    first.into_vec(),
    vec![1, 2, 3],
    "no wait for {DEFAULT_CHUNK_SIZE}"
  );

  // The fourth take was left waiting in the rest: it receives the next offer.
  offer(&q, [4, 5]);
  let (second, s) = ok(s.next_chunk()).expect("a chunk");
  assert_eq!(second.into_vec(), vec![4, 5]);

  ok(q.shutdown::<Never, HNil>());
  assert!(ok(s.next_chunk()).is_none());

  // The RPC client-stream shape: frames that are buffered come in one pull, then it stops at
  // the frame that is not there yet.
  let frames = Frames::default();
  for n in 1..=3 {
    frames.push(Some(n));
  }
  let (chunk, rest) = pull(client_stream(frames.clone()), DEFAULT_CHUNK_SIZE)
    .unwrap()
    .expect("a chunk");
  assert_eq!(chunk, vec![10, 20, 30]);
  frames.push(Some(4));
  frames.push(None);
  let (chunk, rest) = pull(rest, DEFAULT_CHUNK_SIZE).unwrap().expect("a chunk");
  assert_eq!(chunk, vec![40]);
  assert!(pull(rest, DEFAULT_CHUNK_SIZE).unwrap().is_none());
}

/// `run_for_each` on a live stream sees each element as it arrives (TestClock: one element per
/// 10 ms of virtual time) instead of stalling until 64 have.
#[test]
fn run_for_each_live_no_stall() {
  let ticks: Stream<u32> = Stream::unfold_effect(0_u32, |n| {
    sleep(Duration::from_millis(10)).map(move |()| Some((n, n + 1)))
  });
  let (seen, eff) = watch(ticks);
  let (exec, h) = spawn_test(eff);
  assert!(seen_now(&seen).is_empty());
  for k in 1..=5_u32 {
    exec.advance(Duration::from_millis(10));
    assert_eq!(
      seen_now(&seen),
      (0..k).collect::<Vec<_>>(),
      "after {k} ticks"
    );
  }
  h.interrupt_now();
  exec.run_until_stalled();

  // The client-stream shape, driven by frames instead of the clock.
  let frames = Frames::default();
  let (seen, eff) = watch(client_stream(frames.clone()));
  let (exec, h) = spawn_test(eff);
  for n in 1..=4_u32 {
    frames.push(Some(n));
    exec.run_until_stalled();
    assert_eq!(seen_now(&seen), (1..=n).map(|x| x * 10).collect::<Vec<_>>());
  }
  frames.push(None);
  exec.run_until_stalled();
  joined(h);
}

/// Bounded and pure sources, and generators whose steps never wait, keep full chunks.
#[test]
fn bounded_stream_chunking_unchanged() {
  const LEN: usize = 150;
  let expect = full_chunks(LEN, DEFAULT_CHUNK_SIZE);
  let len = LEN as u32;

  let vec_source: Stream<u32> = Stream::from_vec((0..len).collect());
  let iter_source: Stream<u32> = Stream::from_iterable(0..len);
  let unfold: Stream<u32> = Stream::unfold(0_u32, move |n| (n < len).then_some((n, n + 1)));
  let unfold_pure: Stream<u32> =
    Stream::unfold_effect(0_u32, move |n| succeed((n < len).then_some((n, n + 1))));
  let unfold_sync: Stream<u32> = Stream::unfold_effect(0_u32, move |n| {
    sync(move || (n < len).then_some((n, n + 1)))
  });
  let unfold_ready_future: Stream<u32> = Stream::unfold_effect(0_u32, move |n| {
    from_async(move || async move { Ok::<_, Never>((n < len).then_some((n, n + 1))) })
  });
  let unfold_mapped: Stream<u32> = Stream::unfold_effect(0_u32, move |n| {
    from_async(move || async move { Ok::<_, Never>(n) })
      .map(move |n| (n < len).then_some((n, n + 1)))
  });
  let from_effect: Stream<u32> = Stream::from_effect(succeed((0..len).collect()));

  for (name, s) in [
    ("from_vec", vec_source),
    ("from_iterable", iter_source),
    ("unfold", unfold),
    ("unfold_effect(succeed)", unfold_pure),
    ("unfold_effect(sync)", unfold_sync),
    ("unfold_effect(ready future)", unfold_ready_future),
    ("unfold_effect(ready future + map)", unfold_mapped),
    ("from_effect", from_effect),
  ] {
    let got = chunks(s, DEFAULT_CHUNK_SIZE);
    assert_eq!(sizes(&got), expect, "{name}");
    assert_eq!(got.concat(), (0..len).collect::<Vec<_>>(), "{name}");
  }

  // Other requested sizes too, and one element per pull.
  let s: Stream<u32> =
    Stream::unfold_effect(0_u32, move |n| sync(move || (n < 25).then_some((n, n + 1))));
  assert_eq!(sizes(&chunks(s, 10)), vec![10, 10, 5]);
  let s: Stream<u32> = Stream::unfold_effect(0_u32, |n| succeed((n < 3).then_some((n, n + 1))));
  assert_eq!(chunks(s, 1), vec![vec![0], vec![1], vec![2]]);

  // `run_for_each` over a pure source sees every element once.
  let s: Stream<u32> =
    Stream::unfold_effect(0_u32, move |n| succeed((n < len).then_some((n, n + 1))));
  let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
  let items = Arc::new(AtomicUsize::new(0));
  let i = Arc::clone(&items);
  ok(
    s.with_throughput_counter(Arc::clone(&counter))
      .run_for_each(move |_| {
        i.fetch_add(1, Ordering::SeqCst);
        succeed(())
      }),
  );
  assert_eq!(items.load(Ordering::SeqCst), LEN);
  assert_eq!(counter.load(Ordering::SeqCst), LEN as u64);
}

/// `unfold_effect` splits a pull at the first step that would wait, whatever makes it wait, and
/// the next pull resumes that step; ends and failures keep their place in the stream.
#[test]
fn unfold_effect_partial_chunks() {
  // A step that needs the fiber (a yield): not started by the pull that stops on it.
  let started = Arc::new(AtomicUsize::new(0));
  let st = Arc::clone(&started);
  let s: Stream<u32> = Stream::unfold_effect(0_u32, move |n| match n {
    3 => {
      let st = Arc::clone(&st);
      sync(move || st.fetch_add(1, Ordering::SeqCst))
        .then(yield_fiber())
        .map(move |()| Some((n, n + 1)))
    }
    n if n < 5 => succeed(Some((n, n + 1))),
    _ => succeed(None),
  });
  let (first, rest) = pull(s, DEFAULT_CHUNK_SIZE).unwrap().expect("a chunk");
  assert_eq!(first, vec![0, 1, 2]);
  // The synchronous part before the yield ran once and is not run again by the next pull.
  assert_eq!(started.load(Ordering::SeqCst), 1);
  assert_eq!(chunks(rest, DEFAULT_CHUNK_SIZE), vec![vec![3, 4]]);
  assert_eq!(started.load(Ordering::SeqCst), 1);

  // A plain future that is pending, with and without a `map` after it.
  let plain: Stream<u32> = Stream::unfold_effect(0_u32, |n| {
    if n == 2 {
      from_async(move || async move {
        PendingOnce(false).await;
        Ok::<_, Never>(Some((n, n + 1)))
      })
    } else {
      succeed((n < 4).then_some((n, n + 1)))
    }
  });
  assert_eq!(
    chunks(plain, DEFAULT_CHUNK_SIZE),
    vec![vec![0, 1], vec![2, 3]]
  );
  let mapped: Stream<u32> = Stream::unfold_effect(0_u32, |n| {
    if n % 3 == 2 {
      from_async(move || async move {
        PendingOnce(false).await;
        Ok::<_, Never>(n)
      })
      .map(move |n| (n < 7).then_some((n, n + 1)))
    } else {
      sync(move || (n < 7).then_some((n, n + 1)))
    }
  });
  assert_eq!(
    chunks(mapped, DEFAULT_CHUNK_SIZE),
    vec![vec![0, 1], vec![2, 3, 4], vec![5, 6]]
  );

  // The generator ends after a step that waited, and while gathering ready steps.
  let ends: Stream<u32> = Stream::unfold_effect(0_u32, |n| match n {
    0 => succeed(Some((0, 1))),
    1 => yield_fiber().map(|()| None),
    _ => unreachable!("pulled past the end"),
  });
  assert_eq!(chunks(ends, DEFAULT_CHUNK_SIZE), vec![vec![0]]);
  let ends_sync: Stream<u32> =
    Stream::unfold_effect(0_u32, |n| sync(move || (n < 2).then_some((n, n + 1))));
  assert_eq!(chunks(ends_sync, DEFAULT_CHUNK_SIZE), vec![vec![0, 1]]);

  // A failure after a step that waited arrives on the next pull, after the elements before it.
  let late: Stream<u32, &str> = Stream::unfold_effect(0_u32, |n| match n {
    0 => succeed(Some((0, 1))),
    _ => yield_fiber().then(fail("late")),
  });
  let (first, rest) = pull(late, DEFAULT_CHUNK_SIZE).unwrap().expect("a chunk");
  assert_eq!(first, vec![0]);
  assert_eq!(pull(rest, DEFAULT_CHUNK_SIZE).err(), Some("late"));

  // A step that fails without waiting fails the pull, as before (ready and synchronous steps).
  let pure_fail: Stream<u32, &str> = Stream::unfold_effect(0_u32, |n| {
    if n == 0 {
      succeed(Some((0, 1)))
    } else {
      fail("now")
    }
  });
  assert_eq!(pull(pure_fail, DEFAULT_CHUNK_SIZE).err(), Some("now"));
  let sync_fail: Stream<u32, &str> = Stream::unfold_effect(0_u32, |n| {
    if n == 0 {
      succeed(Some((0, 1)))
    } else {
      suspend(|| fail("sync"))
    }
  });
  assert_eq!(pull(sync_fail, DEFAULT_CHUNK_SIZE).err(), Some("sync"));
  let died: Stream<u32> = Stream::unfold_effect(0_u32, |n| {
    if n == 0 {
      succeed(Some((0, 1)))
    } else {
      suspend(|| die("boom"))
    }
  });
  assert!(matches!(
    died.next_chunk().block_on(),
    Exit::Failure(Cause::Die(_))
  ));

  // A pull that fills up while gathering stops there, leaving the next step unbuilt.
  let built = Arc::new(AtomicUsize::new(0));
  let b = Arc::clone(&built);
  let s: Stream<u32> = Stream::unfold_effect(0_u32, move |n| {
    b.fetch_add(1, Ordering::SeqCst);
    sync(move || Some((n, n + 1)))
  });
  let (first, _rest) = pull(s, 3).unwrap().expect("a chunk");
  assert_eq!(first, vec![0, 1, 2]);
  assert_eq!(built.load(Ordering::SeqCst), 3);

  // Ready values after a synchronous step are gathered too.
  let mixed: Stream<u32> = Stream::unfold_effect(0_u32, |n| {
    let next = (n < 6).then_some((n, n + 1));
    if n % 2 == 0 {
      sync(move || next)
    } else {
      succeed(next)
    }
  });
  assert_eq!(
    chunks(mixed, DEFAULT_CHUNK_SIZE),
    vec![vec![0, 1, 2, 3, 4, 5]]
  );

  // An async body that needs the fiber (`from_ctx`, the `effect!` target) is not started by the
  // pull that stops on it.
  let bodies = Arc::new(AtomicUsize::new(0));
  let b = Arc::clone(&bodies);
  let ctx_steps: Stream<u32> = Stream::unfold_effect(0_u32, move |n| {
    if n == 2 {
      let b = Arc::clone(&b);
      Effect::from_ctx(move |_ctx| async move {
        b.fetch_add(1, Ordering::SeqCst);
        Ok(Some((n, n + 1)))
      })
    } else {
      succeed((n < 4).then_some((n, n + 1)))
    }
  });
  let (first, rest) = pull(ctx_steps, DEFAULT_CHUNK_SIZE)
    .unwrap()
    .expect("a chunk");
  assert_eq!(first, vec![0, 1]);
  assert_eq!(bodies.load(Ordering::SeqCst), 0);
  assert_eq!(chunks(rest, DEFAULT_CHUNK_SIZE), vec![vec![2, 3]]);
  assert_eq!(bodies.load(Ordering::SeqCst), 1);
}

/// Like the runloop, a pull polls no future once its fiber is asked to stop: the elements it
/// gathered are dropped with the interrupted pull, and the waiting step never ran.
#[test]
fn unfold_effect_polls_no_future_once_interrupted() {
  let slot: Arc<Mutex<Option<FiberHandle<(), Never>>>> = Arc::default();
  let polled = Arc::new(AtomicUsize::new(0));
  let (to_stop, p) = (Arc::clone(&slot), Arc::clone(&polled));
  let s: Stream<u32> = Stream::unfold_effect(0_u32, move |n| match n {
    0 => succeed(Some((0, 1))),
    1 => {
      let to_stop = Arc::clone(&to_stop);
      sync(move || {
        if let Some(h) = to_stop.lock().unwrap().as_ref() {
          h.interrupt_now();
        }
        Some((1, 2))
      })
    }
    _ => {
      let p = Arc::clone(&p);
      from_async(move || async move {
        p.fetch_add(1, Ordering::SeqCst);
        Ok::<_, Never>(Some((n, n + 1)))
      })
    }
  });
  let exec = TestExecutor::new();
  let h = s.run_drain().fork_root(Arc::new(exec.clone()));
  *slot.lock().unwrap() = Some(h);
  exec.run_until_stalled();
  let h = slot.lock().unwrap().take().expect("the handle");
  assert!(h.is_done(), "the interrupted fiber ended");
  let mut join = pin!(h.join_future());
  let exit = match join.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(exit) => exit,
    Poll::Pending => panic!("join is not ready"),
  };
  assert!(
    matches!(exit, Exit::Failure(Cause::Interrupt(_))),
    "{exit:?}"
  );
  assert_eq!(
    polled.load(Ordering::SeqCst),
    0,
    "no poll after the interrupt"
  );
}
