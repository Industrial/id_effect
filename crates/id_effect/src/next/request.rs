//! Request batching: [`Effect::request`] over a batched [`RequestResolver`] (Effect.ts
//! `Effect.request(req, resolver)` / `RequestResolver.makeBatched`). Filled by
//! `p-request-batch`; `p-request-cache` adds caching scoped to a `Layer` or `Scope`.
//!
//! A [`Request`] is a value that describes one lookup (its key is the value itself, so it is
//! `Hash + Eq`) and names its typed `Output` and `Error`. A [`RequestResolver`] answers a whole
//! batch of distinct requests in one call. Requests to the same resolver made in one *batch
//! window* are deduplicated and sent as a single resolver call; every requester gets its own
//! clone of the result.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect::next::{Concurrency, Effect, Request, RequestResolver, all, sync};
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Hash)]
//! struct GetUser(u32);
//!
//! impl Request for GetUser {
//!   type Output = String;
//!   type Error = String;
//! }
//!
//! let users = RequestResolver::make_batched(|reqs: Vec<GetUser>| {
//!   sync(move || reqs.into_iter().map(|GetUser(id)| Ok(format!("user-{id}"))).collect())
//! });
//!
//! // Three requests started together: one resolver call with the keys {1, 2}.
//! let names = all(
//!   [1, 2, 1].map(|id| Effect::request(GetUser(id), &users)),
//!   Concurrency::Unbounded,
//! );
//! assert!(matches!(names.block_on(), Exit::Success(v) if v == ["user-1", "user-2", "user-1"]));
//! ```
//!
//! # The batch window
//!
//! Effect.ts collects the requests of fibers that were started together, before the scheduler
//! moves on. Here a window is opened by the first request to a resolver that has none open. The
//! request registers synchronously, at the instruction that issues it, and the window's flush
//! runs on its own detached fiber (it inherits the opener's executor, fiber refs and
//! environment, but no requester's interruption). That fiber first yields to its executor,
//! then keeps yielding while the window is still filling, and closes it when either
//!
//! - every requester's parent fiber has as many requesters in the window as it has live
//!   children (all siblings of a [`fork_all`](crate::next::fork_all) are waiting here; a root
//!   requester counts as settled), or
//! - the window has not grown for 2 yields, or
//! - 64 yields have passed (a bound on the latency batching adds).
//!
//! [`all`](crate::next::all) spawns every child before any of them runs, so on the FIFO
//! [`TestExecutor`](crate::next::TestExecutor) the children of one `all` always share a window:
//! batching is deterministic there. On a multi-thread executor the same rule holds in practice
//! but is timing-dependent; batching is an optimization only, and results never depend on how
//! requests were grouped. A request made after its window closed opens a new one, so
//! sequential requests make one resolver call each.
//!
//! # Errors
//!
//! Errors are typed: a resolver returns one `Result<Output, Error>` per request (in the order of
//! the requests it was given), so one request can fail with `Cause::Fail(e)` while the rest of
//! the batch succeeds, and a resolver effect that fails with `e` fails every request of the
//! batch with that cause (cloned). A resolver that returns the wrong number of results, panics,
//! or is dropped before finishing fails every request of the batch with `Cause::Die`.
//!
//! # The cache hook (`p-request-cache`)
//!
//! A request's identity is `(resolver, request)`: [`RequestResolver::id`] (a [`ResolverId`]
//! shared by every clone of one resolver) plus the request value (`Hash + Eq`). Every request
//! goes through one entry point, `Shared::submit`, which returns the shared result slot for the
//! request in the open window. A cache scoped to a `Layer` or `Scope` slots in there, keyed by
//! `(ResolverId, Q)`: a hit returns the cached (completed or in-flight) slot without touching
//! the window, and a miss registers in the window and stores the window's slot in the cache.
//! The window and the flush need no change for that.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use crate::failure::{Cause, Exit};
use crate::next::capability::HNil;
use crate::next::kernel::{Effect, Instr, erase_exit, succeed, suspend, yield_fiber};
use crate::next::runtime::{FiberContext, spawn_fiber};
use crate::runtime::Never;

/// How many yields without a new request close a window that has not settled by sibling count.
const STABLE_ROUNDS: u32 = 2;

/// The most yields a window stays open (a bound on the latency batching adds).
const MAX_ROUNDS: u32 = 64;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A request: one lookup with a typed `Output` and a typed `Error` (Effect.ts `Request<A, E>`).
///
/// The value itself is the request's key: two equal requests to the same resolver in one batch
/// window are resolved once.
pub trait Request: Clone + Eq + Hash + Send + Sync + 'static {
  /// The value a successful request yields (cloned to every requester of an equal request).
  type Output: Clone + Send + Sync + 'static;
  /// The typed failure of a request (cloned to every requester of an equal request).
  type Error: Clone + Send + Sync + 'static;
}

/// The identity of a [`RequestResolver`]: fresh for every constructed resolver, shared by its
/// clones. Together with the request value it is the key a request cache uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResolverId(u64);

impl ResolverId {
  fn fresh() -> Self {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    Self(NEXT.fetch_add(1, Ordering::Relaxed))
  }
}

/// What one resolver call yields: one result per request, in the order of the requests given.
pub type BatchResults<Q> = Vec<Result<<Q as Request>::Output, <Q as Request>::Error>>;

type BatchFn<Q, R> =
  dyn Fn(Vec<Q>) -> Effect<BatchResults<Q>, <Q as Request>::Error, R> + Send + Sync;

/// A requester's outcome, as stored in the shared slot.
type Outcome<Q> = Result<<Q as Request>::Output, Cause<<Q as Request>::Error>>;

/// Resolves batches of requests of type `Q` with an effect that needs the environment `R`
/// (Effect.ts `RequestResolver`). Cloning gives another handle to the same resolver: the same
/// [`id`](Self::id) and the same batch window.
pub struct RequestResolver<Q: Request, R = HNil> {
  shared: Arc<Shared<Q, R>>,
}

impl<Q: Request, R> Clone for RequestResolver<Q, R> {
  fn clone(&self) -> Self {
    Self {
      shared: Arc::clone(&self.shared),
    }
  }
}

impl<Q: Request, R> fmt::Debug for RequestResolver<Q, R> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("RequestResolver")
      .field(&self.shared.id)
      .finish()
  }
}

impl<Q: Request, R: Send + Sync + 'static> RequestResolver<Q, R> {
  /// A resolver that answers every batch with one call of `run` (Effect.ts
  /// `RequestResolver.makeBatched`).
  ///
  /// `run` receives the distinct requests of a batch window (never empty) and returns one result
  /// per request, in the same order. A failure of the returned effect fails every request of
  /// the batch with that cause; see the [module docs](self).
  pub fn make_batched<F>(run: F) -> Self
  where
    F: Fn(Vec<Q>) -> Effect<BatchResults<Q>, Q::Error, R> + Send + Sync + 'static,
  {
    Self {
      shared: Arc::new(Shared {
        id: ResolverId::fresh(),
        run: Box::new(run),
        window: Mutex::new(None),
      }),
    }
  }

  /// This resolver's identity (shared by its clones).
  pub fn id(&self) -> ResolverId {
    self.shared.id
  }
}

/// Issues `req` to `resolver`: joins the resolver's open batch window (or opens one) and waits
/// for the request's result. See [`Effect::request`] and the [module docs](self).
pub fn request<Q, R>(req: Q, resolver: &RequestResolver<Q, R>) -> Effect<Q::Output, Q::Error, R>
where
  Q: Request,
  R: Send + Sync + 'static,
{
  let shared = Arc::clone(&resolver.shared);
  Effect::from_instr(Instr::Async(Box::new(move |env, fiber| {
    let (slot, opened) = shared.submit(req, fiber.parent());
    if opened {
      let ctx = FiberContext::new_detached(fiber);
      // The flush owns its own completion: every slot of the window is completed by it, or
      // with a `Die` when it is dropped unfinished, so its exit cell is not needed.
      let _ = spawn_fiber(&ctx, flush(shared).into_instr(), Arc::clone(env));
    }
    Box::pin(async move { erase_exit(SlotFuture(slot).await) })
  })))
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Issues `req` to `resolver` (Effect.ts `Effect.request(req, resolver)`).
  ///
  /// Requests to the same resolver made in one batch window (for example by the children of one
  /// [`all`](crate::next::all)) are deduplicated and resolved by one resolver call; a request
  /// made after the window closed (for example the next step of a sequential chain) starts a
  /// new call. The resolver's typed errors fail the request with `Cause::Fail`. See the
  /// [module docs](crate::next::request).
  pub fn request<Q>(req: Q, resolver: &RequestResolver<Q, R>) -> Self
  where
    Q: Request<Output = A, Error = E>,
  {
    request(req, resolver)
  }
}

// ── shared state ──────────────────────────────────────────────────────────────────────────

struct Shared<Q: Request, R> {
  id: ResolverId,
  run: Box<BatchFn<Q, R>>,
  /// The open batch window, if any.
  window: Mutex<Option<Window<Q>>>,
}

impl<Q: Request, R> Shared<Q, R> {
  /// Registers `req` (made by a fiber whose parent is `parent`) in the open window, opening one
  /// if none is open, and returns the request's result slot and whether a window was opened.
  ///
  /// This is the single entry point of every request: the cache hook of `p-request-cache`.
  fn submit(&self, req: Q, parent: Option<Arc<FiberContext>>) -> (Arc<Slot<Outcome<Q>>>, bool) {
    let mut window = lock(&self.window);
    let opened = window.is_none();
    let w = window.get_or_insert_with(Window::default);
    w.parents.push(parent);
    let slot = match w.index.entry(req) {
      Entry::Occupied(e) => Arc::clone(&w.slots[*e.get()]),
      Entry::Vacant(e) => {
        let slot = Arc::new(Slot::default());
        w.keys.push(e.key().clone());
        w.slots.push(Arc::clone(&slot));
        e.insert(w.slots.len() - 1);
        slot
      }
    };
    (slot, opened)
  }

  /// The open window's request count and whether it has settled by sibling count.
  fn status(&self) -> (usize, bool) {
    lock(&self.window)
      .as_ref()
      .map_or((0, true), |w| (w.parents.len(), w.settled()))
  }

  /// Closes the open window: its distinct requests, and the batch of slots to complete.
  fn close(&self) -> Option<(Vec<Q>, Batch<Q>)> {
    lock(&self.window)
      .take()
      .map(|w| (w.keys, Batch { slots: w.slots }))
  }
}

/// The requests of one open batch window.
struct Window<Q: Request> {
  /// Distinct requests, in order of first appearance.
  keys: Vec<Q>,
  /// The result slot of each distinct request (parallel to `keys`).
  slots: Vec<Arc<Slot<Outcome<Q>>>>,
  index: HashMap<Q, usize>,
  /// The parent fiber of each requester (`None` for a root fiber), one entry per request.
  parents: Vec<Option<Arc<FiberContext>>>,
}

impl<Q: Request> Default for Window<Q> {
  fn default() -> Self {
    Self {
      keys: Vec::new(),
      slots: Vec::new(),
      index: HashMap::new(),
      parents: Vec::new(),
    }
  }
}

impl<Q: Request> Window<Q> {
  /// `true` when every requester's parent has no live child that is not waiting here.
  fn settled(&self) -> bool {
    let mut waiting: HashMap<*const FiberContext, (usize, &Arc<FiberContext>)> = HashMap::new();
    for p in self.parents.iter().flatten() {
      waiting.entry(Arc::as_ptr(p)).or_insert((0, p)).0 += 1;
    }
    waiting.values().all(|(n, p)| *n >= p.live_children())
  }
}

// ── the flush ─────────────────────────────────────────────────────────────────────────────

/// The flush fiber's program: wait for the window to settle, close it, run the resolver once
/// and complete every slot.
fn flush<Q: Request, R: Send + Sync + 'static>(shared: Arc<Shared<Q, R>>) -> Effect<(), Never, R> {
  let guard = FlushGuard {
    shared: Arc::clone(&shared),
    closed: false,
  };
  yield_fiber()
    .then(settle(shared, 1, 0, 0))
    .flat_map(move |()| {
      let mut guard = guard;
      guard.closed = true;
      let Some((keys, batch)) = guard.shared.close() else {
        return succeed(());
      };
      let shared = Arc::clone(&guard.shared);
      suspend(move || (shared.run)(keys))
        .exit()
        .map(move |exit| batch.complete(exit))
    })
}

/// Yields while the window is still filling (see the module docs for the closing rule).
fn settle<Q: Request, R: Send + Sync + 'static>(
  shared: Arc<Shared<Q, R>>,
  round: u32,
  seen: usize,
  stable: u32,
) -> Effect<(), Never, R> {
  suspend(move || {
    let (n, settled) = shared.status();
    let stable = if n == seen { stable + 1 } else { 0 };
    if settled || stable >= STABLE_ROUNDS || round >= MAX_ROUNDS {
      succeed(())
    } else {
      yield_fiber().flat_map(move |()| settle(shared, round + 1, n, stable))
    }
  })
}

/// Closes the window with `Die` if the flush is dropped before it closed it (its fiber never
/// ran to that point), so no requester waits on a window nobody will flush.
struct FlushGuard<Q: Request, R> {
  shared: Arc<Shared<Q, R>>,
  closed: bool,
}

impl<Q: Request, R> Drop for FlushGuard<Q, R> {
  fn drop(&mut self) {
    if !self.closed {
      // Still this flush's window: a new one opens only after this one is closed.
      drop(self.shared.close());
    }
  }
}

/// The slots of a closed window. Dropped before [`complete`](Self::complete) (the resolver
/// fiber was dropped), every slot dies.
struct Batch<Q: Request> {
  slots: Vec<Arc<Slot<Outcome<Q>>>>,
}

impl<Q: Request> Batch<Q> {
  fn complete(mut self, exit: Exit<BatchResults<Q>, Q::Error>) {
    let slots = std::mem::take(&mut self.slots);
    match exit {
      Exit::Success(results) if results.len() == slots.len() => {
        for (slot, r) in slots.iter().zip(results) {
          slot.complete(r.map_err(Cause::Fail));
        }
      }
      Exit::Success(results) => {
        let msg = format!(
          "next: request resolver returned {} results for {} requests",
          results.len(),
          slots.len()
        );
        for slot in &slots {
          slot.complete(Err(Cause::Die(msg.as_str().into())));
        }
      }
      Exit::Failure(cause) => {
        for slot in &slots {
          slot.complete(Err(cause.clone()));
        }
      }
    }
  }
}

impl<Q: Request> Drop for Batch<Q> {
  fn drop(&mut self) {
    for slot in self.slots.drain(..) {
      slot.complete(Err(Cause::Die(
        "next: request batch dropped before its resolver finished".into(),
      )));
    }
  }
}

// ── result slots ──────────────────────────────────────────────────────────────────────────

/// A write-once result shared by every requester of one distinct request.
struct Slot<T> {
  state: Mutex<SlotState<T>>,
}

struct SlotState<T> {
  value: Option<T>,
  wakers: Vec<Waker>,
}

impl<T> Default for Slot<T> {
  fn default() -> Self {
    Self {
      state: Mutex::new(SlotState {
        value: None,
        wakers: Vec::new(),
      }),
    }
  }
}

impl<T: Clone> Slot<T> {
  /// Stores `v` (a second completion is ignored) and wakes every waiter.
  fn complete(&self, v: T) {
    let wakers = {
      let mut s = lock(&self.state);
      if s.value.is_some() {
        return;
      }
      s.value = Some(v);
      std::mem::take(&mut s.wakers)
    };
    for w in wakers {
      w.wake();
    }
  }

  fn poll_get(&self, cx: &mut Context<'_>) -> Poll<T> {
    let mut s = lock(&self.state);
    if let Some(v) = &s.value {
      return Poll::Ready(v.clone());
    }
    if !s.wakers.iter().any(|w| w.will_wake(cx.waker())) {
      s.wakers.push(cx.waker().clone());
    }
    Poll::Pending
  }
}

/// One requester's wait on a [`Slot`].
struct SlotFuture<T>(Arc<Slot<T>>);

impl<T: Clone> Future for SlotFuture<T> {
  type Output = T;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
    self.0.poll_get(cx)
  }
}
