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
//! # Caching (Effect.ts `Request.makeCache` / `Effect.withRequestCache`)
//!
//! Batching deduplicates within one window only. A [`RequestCache`] remembers results across
//! windows: while caching is on, a request whose `(resolver, request)` key is in the ambient
//! cache is answered from it (completed, or still in flight: the requester waits on the same
//! result) without reaching the resolver, and a miss joins the batch window as usual and is
//! stored in the cache. A request's key is [`RequestResolver::id`] (a [`ResolverId`] shared by
//! the clones of one resolver) plus the request value.
//!
//! Caching is **off by default** (Effect.ts parity). The ambient cache and the on/off flag are
//! fiber refs, so they are scoped and inherited by forked children:
//!
//! - [`Effect::with_request_cache`] / [`RequestCache::provide`] run an effect with a cache
//!   (caching on), restoring the previous setting however it ends;
//! - [`RequestCache::layer`] is a `Layer` (Effect.ts `Layer.setRequestCache`) and
//!   [`RequestCache::provide_scoped`] the [`Scope`] form: the cache is in use for the rest of the
//!   fiber until the build's (or the given) scope closes;
//! - [`Effect::with_request_caching`] switches caching on or off for an effect (`true` uses the
//!   ambient cache: the one provided, or a process-wide default cache).
//!
//! ```
//! use id_effect::Exit;
//! use id_effect::next::{Effect, Request, RequestCache, RequestResolver, sync};
//! use std::sync::Arc;
//! use std::sync::atomic::{AtomicUsize, Ordering};
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Hash)]
//! struct GetUser(u32);
//!
//! impl Request for GetUser {
//!   type Output = String;
//!   type Error = String;
//! }
//!
//! let calls = Arc::new(AtomicUsize::new(0));
//! let seen = Arc::clone(&calls);
//! let users = RequestResolver::make_batched(move |reqs: Vec<GetUser>| {
//!   seen.fetch_add(1, Ordering::SeqCst);
//!   sync(move || reqs.into_iter().map(|GetUser(id)| Ok(format!("user-{id}"))).collect())
//! });
//!
//! let twice = |users: &RequestResolver<GetUser>| {
//!   let again = users.clone();
//!   Effect::request(GetUser(1), users).flat_map(move |_| Effect::request(GetUser(1), &again))
//! };
//!
//! // Caching off (the default): two sequential requests, two resolver calls.
//! assert!(matches!(twice(&users).block_on(), Exit::Success(_)));
//! assert_eq!(calls.load(Ordering::SeqCst), 2);
//!
//! // With a cache: the second request is answered from it.
//! let cache = RequestCache::new();
//! assert!(matches!(twice(&users).with_request_cache(&cache).block_on(), Exit::Success(_)));
//! assert_eq!(calls.load(Ordering::SeqCst), 3);
//! assert_eq!(cache.len(), 1);
//! ```
//!
//! **Failures are cached** (Effect.ts caches the request's `Exit`): a request that failed with
//! `Cause::Fail(e)`, or died (including a batch whose resolver fiber was dropped), keeps that
//! outcome in the cache. Use a fresh cache, a narrower scope or [`RequestCache::clear`] to
//! retry. A cache holds at most [`capacity`](RequestCache::capacity) entries and evicts the
//! oldest first; it has no time-to-live.

use std::any::Any;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll, Waker};

use crate::failure::{Cause, Exit};
use crate::next::capability::HNil;
use crate::next::kernel::{Effect, Instr, erase, erase_exit, succeed, suspend, sync, yield_fiber};
use crate::next::layer::Layer;
use crate::next::runtime::{FiberContext, FiberRef, spawn_fiber};
use crate::next::scope::Scope;
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
    let (slot, opened) = match RequestCache::current_in(fiber) {
      Some(cache) => cache.lookup_or_submit(&shared, req, fiber.parent()),
      None => shared.submit(req, fiber.parent()),
    };
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

  /// Runs this effect with `cache` as the ambient request cache and caching on (Effect.ts
  /// `Effect.withRequestCache(cache)`, here also enabling caching); the previous setting is
  /// restored however it ends. Same as [`RequestCache::provide`].
  pub fn with_request_cache(self, cache: &RequestCache) -> Self {
    cache.provide(self)
  }

  /// Runs this effect with request caching on or off (Effect.ts
  /// `Effect.withRequestCaching(enabled)`); the previous setting is restored however it ends.
  /// On, requests use the ambient cache: the one provided by an enclosing
  /// [`with_request_cache`](Self::with_request_cache) / [`RequestCache::layer`] /
  /// [`RequestCache::provide_scoped`], else a process-wide default cache (of
  /// [`DEFAULT_REQUEST_CACHE_CAPACITY`] entries).
  pub fn with_request_caching(self, enabled: bool) -> Self {
    caching_ref().locally(enabled, self)
  }
}

// ── request caching ───────────────────────────────────────────────────────────────────────

/// The capacity of a [`RequestCache::new`] cache and of the process-wide default cache (Effect.ts
/// `Request.makeCache` default).
pub const DEFAULT_REQUEST_CACHE_CAPACITY: usize = 65_536;

/// A cache of request results keyed by `(resolver, request)` (Effect.ts `Request.Cache`, made
/// with `Request.makeCache`). Cloning gives another handle to the same cache. See the
/// [module docs](self) for how a cache is put in scope.
#[derive(Clone)]
pub struct RequestCache {
  inner: Arc<CacheInner>,
}

struct CacheInner {
  capacity: usize,
  state: Mutex<CacheState>,
}

#[derive(Default)]
struct CacheState {
  /// One typed map per resolver (a [`TypedMap<Q>`] for the resolver's `Q`).
  maps: HashMap<ResolverId, Box<dyn ErasedMap>>,
  /// The resolver of every entry, oldest first (each map keeps its own entries in that order).
  order: VecDeque<ResolverId>,
}

/// A resolver's entries, type-erased.
trait ErasedMap: Send {
  fn as_any_mut(&mut self) -> &mut dyn Any;
  /// Removes the oldest entry; `true` when the map is then empty.
  fn evict_oldest(&mut self) -> bool;
}

struct TypedMap<Q: Request> {
  slots: HashMap<Q, Arc<Slot<Outcome<Q>>>>,
  order: VecDeque<Q>,
}

impl<Q: Request> ErasedMap for TypedMap<Q> {
  fn as_any_mut(&mut self) -> &mut dyn Any {
    self
  }

  fn evict_oldest(&mut self) -> bool {
    if let Some(q) = self.order.pop_front() {
      self.slots.remove(&q);
    }
    self.order.is_empty()
  }
}

impl Default for RequestCache {
  fn default() -> Self {
    Self::new()
  }
}

impl fmt::Debug for RequestCache {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RequestCache")
      .field("len", &self.len())
      .field("capacity", &self.inner.capacity)
      .finish()
  }
}

impl RequestCache {
  /// An empty cache of [`DEFAULT_REQUEST_CACHE_CAPACITY`] entries (Effect.ts
  /// `Request.makeCache`).
  pub fn new() -> Self {
    Self::with_capacity(DEFAULT_REQUEST_CACHE_CAPACITY)
  }

  /// An empty cache of at most `capacity` entries (`0` counts as `1`); when full, the oldest
  /// entry is evicted.
  pub fn with_capacity(capacity: usize) -> Self {
    Self {
      inner: Arc::new(CacheInner {
        capacity: capacity.max(1),
        state: Mutex::default(),
      }),
    }
  }

  /// The most entries this cache holds.
  pub fn capacity(&self) -> usize {
    self.inner.capacity
  }

  /// The number of cached requests (completed or in flight).
  pub fn len(&self) -> usize {
    lock(&self.inner.state).order.len()
  }

  /// `true` when nothing is cached.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Forgets every entry (requests in flight still complete for their current requesters).
  pub fn clear(&self) {
    *lock(&self.inner.state) = CacheState::default();
  }

  /// Runs `eff` with `self` as the ambient request cache and caching on; the previous setting
  /// is restored however `eff` ends, and children `eff` forks inherit it.
  pub fn provide<A, E, R>(&self, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    cache_ref().locally(self.clone(), caching_ref().locally(true, eff))
  }

  /// Makes `self` the current fiber's ambient request cache, with caching on, until `scope`
  /// closes (Effect.ts `FiberRef.locallyScoped`): closing it restores this fiber's previous
  /// setting. Children forked meanwhile inherit the cache.
  pub fn provide_scoped<E: Send + 'static, R: Send + Sync + 'static>(
    &self,
    scope: &Scope,
  ) -> Effect<(), E, R> {
    let cache = self.clone();
    let scope = scope.clone();
    Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      let prev_cache = cache_ref().get_in(fiber);
      let prev_on = caching_ref().get_in(fiber);
      let (here, there) = (Arc::clone(fiber), Arc::clone(fiber));
      // Register the restore first: an interrupt between the two steps restores a value that
      // was never replaced, which is harmless.
      let restore = sync(move || {
        cache_ref().set_in(&there, prev_cache);
        caching_ref().set_in(&there, prev_on);
      });
      scope
        .add_finalizer::<E, R>(restore)
        .map(move |()| {
          cache_ref().set_in(&here, cache);
          caching_ref().set_in(&here, true);
        })
        .into_instr()
    })))
  }

  /// A layer that makes `self` the ambient request cache, with caching on, for the effect it is
  /// provided to (Effect.ts `Layer.setRequestCache`, here also enabling caching). It outputs
  /// nothing (`HNil`): merge it into an application layer, or provide it alone. The previous
  /// setting is restored when the build's scope closes (after the effect a
  /// [`Effect::provide_layer`] runs); a plain [`Layer::build`] never closes it.
  pub fn layer<E: Send + 'static>(&self) -> Layer<HNil, E> {
    let cache = self.clone();
    Layer::scoped(move |scope| cache.provide_scoped::<E, HNil>(&scope).map(|()| HNil))
  }

  /// The cache in use by the current fiber: `Some` while caching is on.
  pub fn current<E: Send + 'static, R: Send + Sync + 'static>() -> Effect<Option<Self>, E, R> {
    Effect::from_instr(Instr::WithFiber(Box::new(|_, fiber| {
      Instr::Pure(Ok(erase(Self::current_in(fiber))))
    })))
  }

  /// The cache in use by `fiber`, outside an effect: `Some` while caching is on.
  pub fn current_in(fiber: &FiberContext) -> Option<Self> {
    caching_ref()
      .get_in(fiber)
      .then(|| cache_ref().get_in(fiber))
  }

  /// The cached slot of `req` to `shared`'s resolver, or (on a miss) the slot of its window
  /// registration, now cached. Holds the cache lock across the window registration, so two
  /// requesters of one key never both miss (lock order: cache, then window).
  fn lookup_or_submit<Q: Request, R>(
    &self,
    shared: &Shared<Q, R>,
    req: Q,
    parent: Option<Arc<FiberContext>>,
  ) -> (Arc<Slot<Outcome<Q>>>, bool) {
    let mut state = lock(&self.inner.state);
    let CacheState { maps, order } = &mut *state;
    let map = maps.entry(shared.id).or_insert_with(|| {
      Box::new(TypedMap::<Q> {
        slots: HashMap::new(),
        order: VecDeque::new(),
      })
    });
    let Some(typed) = map.as_any_mut().downcast_mut::<TypedMap<Q>>() else {
      unreachable!("next: request cache entry of a foreign request type (kernel bug)")
    };
    if let Some(slot) = typed.slots.get(&req) {
      return (Arc::clone(slot), false);
    }
    let (slot, opened) = shared.submit(req.clone(), parent);
    typed.slots.insert(req.clone(), Arc::clone(&slot));
    typed.order.push_back(req);
    order.push_back(shared.id);
    while order.len() > self.inner.capacity {
      let Some(id) = order.pop_front() else { break };
      if maps.get_mut(&id).is_some_and(|m| m.evict_oldest()) {
        maps.remove(&id);
      }
    }
    (slot, opened)
  }
}

/// Whether request caching is on in a fiber (off by default).
fn caching_ref() -> &'static FiberRef<bool> {
  static ON: OnceLock<FiberRef<bool>> = OnceLock::new();
  ON.get_or_init(|| FiberRef::new(false))
}

/// A fiber's ambient request cache (the process-wide default cache until one is provided).
fn cache_ref() -> &'static FiberRef<RequestCache> {
  static CACHE: OnceLock<FiberRef<RequestCache>> = OnceLock::new();
  CACHE.get_or_init(|| FiberRef::new(RequestCache::new()))
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
