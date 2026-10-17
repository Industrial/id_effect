//! The fiber registry (feature `fiber-registry`, 0.10 D8): every live [`FiberContext`] in the
//! process, for a console's fiber dump ([`fiber_dump`]).
//!
//! A context registers itself when it is built and unregisters exactly once, where it leaves the
//! live-fiber gauge ([`runtime_metrics`](crate::observability::runtime_metrics)): when it is
//! marked done or, if it never completes through a run edge, when it is dropped. The map holds
//! a `Weak` reference and the start instant per fiber, so the registry never keeps a fiber alive
//! and `FiberContext` does not grow; it is sharded by fiber id (64 shards behind their own
//! mutex), so forks on different threads rarely meet on one lock.
//!
//! Cost with the feature on, per fiber: one `Instant::now()` and one shard lock + map insert
//! when it is built, one shard lock + map remove when it ends. Without the feature this module
//! is not compiled and the calls into it do not exist.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use crate::failure::FiberId;
use crate::observability::Span;
use crate::runtime::FiberContext;

const SHARDS: usize = 64;

/// A registered fiber and when it was built.
type Entry = (Weak<FiberContext>, Instant);

type Shard = Mutex<HashMap<FiberId, Entry>>;

static REGISTRY: LazyLock<[Shard; SHARDS]> =
  LazyLock::new(|| std::array::from_fn(|_| Mutex::new(HashMap::new())));

fn shard(id: FiberId) -> MutexGuard<'static, HashMap<FiberId, Entry>> {
  // Ids are sequential, so consecutive forks land on consecutive shards.
  REGISTRY[(id.as_u64() % SHARDS as u64) as usize]
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
}

/// Lists `ctx` (called once, when the context is built). The start instant is on the process
/// clock: building a fiber must not call into its executor (a pure effect never touches it).
pub(crate) fn register(ctx: &Arc<FiberContext>) {
  let entry = (Arc::downgrade(ctx), Instant::now());
  shard(ctx.id()).insert(ctx.id(), entry);
}

/// Removes fiber `id` (called once, when it is marked done or dropped uncounted).
pub(crate) fn unregister(id: FiberId) {
  shard(id).remove(&id);
}

/// What a fiber is doing, as far as the runtime knows without stopping it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FiberState {
  /// Running or suspended, no interrupt requested.
  Running,
  /// An interrupt was requested and the fiber has not finished yet (it may be in a masked
  /// region, or running its finalizers).
  Interrupting,
  /// Marked done between the dump reading the registry and reading the fiber.
  Done,
}

/// One live fiber in a [`fiber_dump`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FiberInfo {
  /// The fiber's id.
  pub id: FiberId,
  /// The forking fiber, while it is alive (`None` for roots, daemons and `Scope` forks).
  pub parent: Option<FiberId>,
  /// The children forked with `Effect::fork` that have not completed yet, by id.
  pub children: Vec<FiberId>,
  /// Running, interrupting or (in a race with completion) done.
  pub state: FiberState,
  /// When the fiber was built, on the process clock (`Instant::now()`, not the executor's
  /// clock, which may be virtual: building a fiber never calls into its executor).
  pub started_at: Instant,
  /// How long ago it was built, at dump time, on the same clock.
  pub age: Duration,
  /// The span names from the current span outwards (empty outside any span).
  pub spans: Vec<String>,
}

impl FiberInfo {
  fn of(ctx: &FiberContext, started_at: Instant) -> Self {
    let state = if ctx.is_done() {
      FiberState::Done
    } else if ctx.is_interrupt_requested() {
      FiberState::Interrupting
    } else {
      FiberState::Running
    };
    let mut children = ctx.child_ids();
    children.sort_unstable();
    let spans = std::iter::successors(Span::current_in(ctx), |s| s.parent().cloned())
      .map(|s| s.name().to_owned())
      .collect();
    Self {
      id: ctx.id(),
      parent: ctx.parent().map(|p| p.id()),
      children,
      state,
      started_at,
      age: Instant::now().saturating_duration_since(started_at),
      spans,
    }
  }

  /// The name of the fiber's current span, if it is inside one.
  pub fn span(&self) -> Option<&str> {
    self.spans.first().map(String::as_str)
  }
}

/// Every live fiber in the process, by ascending id: built and not yet done or dropped, on any
/// runtime or executor. Each entry is read without stopping its fiber, so it is a snapshot per
/// fiber, not a consistent cut of the whole tree.
///
/// Needs the `fiber-registry` cargo feature (off by default).
pub fn fiber_dump() -> Vec<FiberInfo> {
  let mut live = Vec::new();
  for shard in REGISTRY.iter() {
    let map = shard.lock().unwrap_or_else(PoisonError::into_inner);
    live.extend(
      map
        .values()
        .filter_map(|(weak, at)| Some((weak.upgrade()?, *at))),
    );
  }
  // No shard lock is held from here on: dropping the last `Arc` of a fiber that was never marked
  // done unregisters it, which locks its shard.
  let mut out: Vec<FiberInfo> = live
    .iter()
    .map(|(ctx, at)| FiberInfo::of(ctx, *at))
    .collect();
  drop(live);
  out.sort_unstable_by_key(|f| f.id);
  out
}

#[cfg(test)]
mod tests {
  use std::sync::mpsc;

  use super::*;
  use crate::{HNil, Never, Runtime, sleep, sync};

  fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
      assert!(Instant::now() < deadline, "timed out waiting for {what}");
      std::thread::sleep(Duration::from_millis(1));
    }
  }

  fn find(id: FiberId) -> Option<FiberInfo> {
    fiber_dump().into_iter().find(|f| f.id == id)
  }

  #[test]
  fn dump_lists_running_and_drops_finished() {
    let (tx, rx) = mpsc::channel();
    let running = Runtime::global().fork(
      sync::<_, Never, HNil>(move || tx.send(()).unwrap())
        .then(sleep(Duration::from_secs(3600)))
        .with_span("outer-work")
        .with_span("request"),
    );
    let finished = Runtime::global().fork(sync::<_, Never, HNil>(|| 7_u32));
    rx.recv_timeout(Duration::from_secs(10))
      .expect("forked fiber never ran");
    wait_until("the short fiber to finish", || finished.fiber().is_done());

    let info = find(running.fiber().id()).expect("running fiber missing from the dump");
    assert_eq!(info.state, FiberState::Running);
    assert_eq!(info.parent, None);
    assert!(info.children.is_empty());
    assert_eq!(info.span(), Some("outer-work"));
    assert_eq!(info.spans, ["outer-work", "request"]);
    assert!(info.age <= Duration::from_secs(3600));
    assert!(
      find(finished.fiber().id()).is_none(),
      "a finished fiber stays listed"
    );

    running.interrupt_now();
    wait_until("the running fiber to finish", || running.fiber().is_done());
    assert!(
      find(running.fiber().id()).is_none(),
      "an interrupted fiber stays listed"
    );
  }

  #[test]
  fn parent_links_follow_fork() {
    let (tx, rx) = mpsc::channel();
    let parent = Runtime::global().fork(
      sleep::<Never, HNil>(Duration::from_secs(3600))
        .fork()
        .flat_map(move |child| {
          tx.send(child.fiber().id()).unwrap();
          sleep(Duration::from_secs(3600))
        }),
    );
    let child = rx
      .recv_timeout(Duration::from_secs(10))
      .expect("parent never forked");
    let parent_id = parent.fiber().id();

    let p = find(parent_id).expect("parent missing from the dump");
    assert_eq!(p.children, [child]);
    let c = find(child).expect("child missing from the dump");
    assert_eq!(c.parent, Some(parent_id));
    assert!(c.started_at >= p.started_at);
    assert!(c.spans.is_empty());

    parent.interrupt_now();
    wait_until("the parent to finish", || parent.fiber().is_done());
    let dump = fiber_dump();
    assert!(!dump.iter().any(|f| f.id == parent_id || f.id == child));
  }
}
