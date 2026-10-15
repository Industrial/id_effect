//! Shared workloads for the w2-spike-gate referee probes. Each `*_async_chain(n)` is exactly the
//! `gate/async_bind_64` body with 64 replaced by `n` (same constructors, same edge as the
//! respective gate bench).

pub mod v06 {
  use id_effect::{Effect, from_async, pure, succeed};

  pub fn async_chain(n: usize) -> usize {
    let mut effect: Effect<usize, (), ()> = pure(0);
    for _ in 0..n {
      effect = effect.flat_map(|n| from_async(move |_r: &mut ()| async move { Ok(n + 1) }));
    }
    pollster::block_on(effect.run(&mut ())).expect("v06 chain")
  }

  pub fn sync_chain(n: usize) -> usize {
    let mut effect: Effect<usize, (), ()> = pure(0);
    for _ in 0..n {
      effect = effect.flat_map(|n| succeed(std::hint::black_box(n) + 1));
    }
    pollster::block_on(effect.run(&mut ())).expect("v06 sync chain")
  }

  /// Build `succeed(1).flat_map(f)` and `.map(g)` without running; return how often f/g ran.
  pub fn laziness() -> (usize, usize) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static F: AtomicUsize = AtomicUsize::new(0);
    static G: AtomicUsize = AtomicUsize::new(0);
    let e1: Effect<usize, (), ()> = succeed::<usize, (), ()>(1).flat_map(|x| {
      F.fetch_add(1, Ordering::SeqCst);
      succeed(x)
    });
    let e2: Effect<usize, (), ()> = succeed::<usize, (), ()>(1).map(|x| {
      G.fetch_add(1, Ordering::SeqCst);
      x
    });
    let r = (F.load(Ordering::SeqCst), G.load(Ordering::SeqCst));
    drop((e1, e2));
    r
  }
}

pub mod a {
  use kernel_a::{Effect, from_async, succeed};

  pub fn async_chain(n: usize) -> usize {
    let mut effect: Effect<usize, ()> = succeed(0);
    for _ in 0..n {
      effect = effect.flat_map(|n| from_async(move || async move { Ok(n + 1) }));
    }
    effect.block_on().expect("a chain")
  }

  pub fn sync_chain(n: usize) -> usize {
    let mut effect: Effect<usize, ()> = succeed(0);
    for _ in 0..n {
      effect = effect.flat_map(|n| succeed(std::hint::black_box(n) + 1));
    }
    effect.block_on().expect("a sync chain")
  }

  pub fn laziness() -> (usize, usize) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static F: AtomicUsize = AtomicUsize::new(0);
    static G: AtomicUsize = AtomicUsize::new(0);
    let e1: Effect<usize, ()> = succeed(1).flat_map(|x| {
      F.fetch_add(1, Ordering::SeqCst);
      succeed(x)
    });
    let e2: Effect<usize, ()> = succeed(1).map(|x| {
      G.fetch_add(1, Ordering::SeqCst);
      x
    });
    let r = (F.load(Ordering::SeqCst), G.load(Ordering::SeqCst));
    drop((e1, e2));
    r
  }
}

pub mod b {
  use kernel_b::{Effect, from_async, succeed};

  pub fn async_chain(n: usize) -> usize {
    let mut effect: Effect<usize, (), ()> = succeed(0);
    for _ in 0..n {
      effect = effect.flat_map(|n| from_async(async move { Ok(n + 1) }));
    }
    effect.block_on().expect("b chain")
  }

  pub fn sync_chain(n: usize) -> usize {
    let mut effect: Effect<usize, (), ()> = succeed(0);
    for _ in 0..n {
      effect = effect.flat_map(|n| succeed(std::hint::black_box(n) + 1));
    }
    effect.block_on().expect("b sync chain")
  }

  pub fn laziness() -> (usize, usize) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static F: AtomicUsize = AtomicUsize::new(0);
    static G: AtomicUsize = AtomicUsize::new(0);
    let e1: Effect<usize, (), ()> = succeed::<usize, (), ()>(1).flat_map(|x| {
      F.fetch_add(1, Ordering::SeqCst);
      succeed(x)
    });
    let e2: Effect<usize, (), ()> = succeed::<usize, (), ()>(1).map(|x| {
      G.fetch_add(1, Ordering::SeqCst);
      x
    });
    let r = (F.load(Ordering::SeqCst), G.load(Ordering::SeqCst));
    drop((e1, e2));
    r
  }
}
