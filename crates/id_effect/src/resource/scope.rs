//! Scope lifecycle primitives for structured resource management.

use crate::coordination::latch::Latch;
use crate::failure::exit::Exit;
use crate::kernel::Effect;
use crate::runtime::{Never, run_blocking};
use crate::stm::{Outcome, Stm, TRef, commit};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

/// One-shot cleanup hook run when a [`Scope`] closes; receives the close [`Exit`] and returns an effect.
pub type Finalizer = Box<dyn FnOnce(Exit<(), Never>) -> Effect<(), Never, ()> + Send + 'static>;

type FinalizerCell = Arc<Mutex<Option<Finalizer>>>;

/// [`Finalizer`] is [`FnOnce`]; cells are [`Arc`] so [`TRef`] transactions can retry idempotently
/// (see `add_finalizer`).
#[derive(Clone, Default)]
struct FinalizerBucket {
  entries: Vec<FinalizerCell>,
}

/// Hierarchical resource scope: parent close cascades to children; supports finalizers and awaitable close.
#[derive(Clone)]
pub struct Scope {
  inner: Arc<ScopeInner>,
}

struct ScopeInner {
  closed: AtomicBool,
  /// Opened after the first successful close (finalizers finished). Idempotent open.
  close_latch: Latch,
  parent: Mutex<Option<Weak<ScopeInner>>>,
  children: Mutex<Vec<Weak<ScopeInner>>>,
  finalizers: TRef<FinalizerBucket>,
}

impl Scope {
  /// Creates a new root scope (open, no parent).
  pub fn make() -> Self {
    let close_latch = run_blocking(Latch::make(), ()).expect("Latch::make");
    let finalizers =
      run_blocking(commit(TRef::make(FinalizerBucket::default())), ()).expect("finalizers tref");
    Self {
      inner: Arc::new(ScopeInner {
        closed: AtomicBool::new(false),
        close_latch,
        parent: Mutex::new(None),
        children: Mutex::new(Vec::new()),
        finalizers,
      }),
    }
  }

  /// Child scope linked under `self`; inherits close if the parent is already closed.
  pub fn fork(&self) -> Self {
    let close_latch = run_blocking(Latch::make(), ()).expect("Latch::make");
    let finalizers =
      run_blocking(commit(TRef::make(FinalizerBucket::default())), ()).expect("finalizers tref");
    let child = Self {
      inner: Arc::new(ScopeInner {
        closed: AtomicBool::new(false),
        close_latch,
        parent: Mutex::new(Some(Arc::downgrade(&self.inner))),
        children: Mutex::new(Vec::new()),
        finalizers,
      }),
    };
    self
      .inner
      .children
      .lock()
      .expect("scope children mutex poisoned")
      .push(Arc::downgrade(&child.inner));
    if self.is_closed() {
      child.close_with_exit(Exit::succeed(()));
    }
    child
  }

  /// Reparents `other` under `self` if both are open; returns `false` if either is closed.
  pub fn extend(&self, other: &Scope) -> bool {
    if self.is_closed() || other.is_closed() {
      return false;
    }
    *other
      .inner
      .parent
      .lock()
      .expect("scope parent mutex poisoned") = Some(Arc::downgrade(&self.inner));
    self
      .inner
      .children
      .lock()
      .expect("scope children mutex poisoned")
      .push(Arc::downgrade(&other.inner));
    true
  }

  /// Closes with [`Exit::succeed`]; returns `true` on the first successful close.
  pub fn close(&self) -> bool {
    self.close_with_exit(Exit::succeed(()))
  }

  /// Closes this scope and descendants, runs finalizers (LIFO) with `exit`, then opens the close latch.
  pub fn close_with_exit(&self, exit: Exit<(), Never>) -> bool {
    if self.inner.closed.swap(true, Ordering::SeqCst) {
      return false;
    }
    let children = self
      .inner
      .children
      .lock()
      .expect("scope children mutex poisoned")
      .iter()
      .filter_map(Weak::upgrade)
      .map(|inner| Scope { inner })
      .collect::<Vec<_>>();
    for child in children {
      child.close_with_exit(exit.clone());
    }

    let drained = run_blocking(
      commit({
        let tr = self.inner.finalizers.clone();
        Stm::from_fn(move |txn| {
          let mut bucket = match tr.read_stm::<()>().run_on(txn) {
            Outcome::Done(b) => b,
            Outcome::Fail(e) => return Outcome::Fail(e),
            Outcome::Retry => return Outcome::Retry,
          };
          let mut drained = Vec::new();
          for cell in bucket.entries.drain(..) {
            if let Some(f) = cell.lock().expect("finalizer cell poisoned").take() {
              drained.push(f);
            }
          }
          match tr.write_stm(FinalizerBucket::default()).run_on(txn) {
            Outcome::Done(()) => Outcome::Done(drained),
            Outcome::Fail(e) => Outcome::Fail(e),
            Outcome::Retry => Outcome::Retry,
          }
        })
      }),
      (),
    )
    .expect("drain finalizers");
    for finalizer in drained.into_iter().rev() {
      let _ = run_blocking(finalizer(exit.clone()), ());
    }
    let _ = run_blocking(self.inner.close_latch.open(), ());
    true
  }

  /// Registers `finalizer` to run on close; returns `false` if already closed.
  pub fn add_finalizer(&self, finalizer: Finalizer) -> bool {
    if self.is_closed() {
      return false;
    }
    let cell: FinalizerCell = Arc::new(Mutex::new(Some(finalizer)));
    run_blocking(
      commit({
        let tr = self.inner.finalizers.clone();
        let cell = Arc::clone(&cell);
        Stm::from_fn(move |txn| {
          let mut bucket = match tr.read_stm::<()>().run_on(txn) {
            Outcome::Done(b) => b,
            Outcome::Fail(e) => return Outcome::Fail(e),
            Outcome::Retry => return Outcome::Retry,
          };
          if !bucket.entries.iter().any(|e| Arc::ptr_eq(e, &cell)) {
            bucket.entries.push(Arc::clone(&cell));
          }
          match tr.write_stm(bucket).run_on(txn) {
            Outcome::Done(()) => Outcome::Done(()),
            Outcome::Fail(e) => Outcome::Fail(e),
            Outcome::Retry => Outcome::Retry,
          }
        })
      }),
      (),
    )
    .expect("push finalizer");
    true
  }

  /// Wait until this scope has completed a successful close (including this scope’s finalizers).
  pub fn wait_closed(&self) -> Effect<(), Never, ()> {
    self.inner.close_latch.wait()
  }

  /// `true` once this scope has fired close signalling (after finalizers on the closing path).
  pub fn close_signalled(&self) -> Effect<bool, Never, ()> {
    self.inner.close_latch.is_open()
  }

  /// `true` if this scope or an ancestor has closed.
  pub fn is_closed(&self) -> bool {
    if self.inner.closed.load(Ordering::SeqCst) {
      return true;
    }
    let parent = self
      .inner
      .parent
      .lock()
      .expect("scope parent mutex poisoned")
      .as_ref()
      .and_then(Weak::upgrade);
    parent
      .map(|inner| Scope { inner }.is_closed())
      .unwrap_or(false)
  }
}

impl Default for Scope {
  fn default() -> Self {
    Self::make()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;
  use std::sync::Arc;

  mod hierarchy {
    use super::*;

    #[test]
    fn fork_with_nested_children_closes_descendants_when_parent_closes() {
      let parent = Scope::make();
      let child = parent.fork();
      let grandchild = child.fork();

      assert!(!parent.is_closed());
      assert!(!child.is_closed());
      assert!(!grandchild.is_closed());

      assert!(parent.close());
      assert!(parent.is_closed());
      assert!(child.is_closed());
      assert!(grandchild.is_closed());
    }

    #[test]
    fn extend_with_open_scopes_attaches_scope_under_new_parent() {
      let parent = Scope::make();
      let detached = Scope::make();
      assert!(parent.extend(&detached));
      parent.close();
      assert!(detached.is_closed());
    }

    #[rstest]
    #[case::closed_parent(true, false)]
    #[case::closed_child(false, true)]
    fn extend_with_closed_scope_returns_false(
      #[case] close_parent: bool,
      #[case] close_child: bool,
    ) {
      let parent = Scope::make();
      let child = Scope::make();
      if close_parent {
        parent.close();
      }
      if close_child {
        child.close();
      }
      assert!(!parent.extend(&child));
    }
  }

  mod closing {
    use super::*;
    use crate::runtime::run_async;

    #[tokio::test]
    async fn scope_close_wakes_all_waiters() {
      let scope = Scope::make();
      let s1 = scope.clone();
      let s2 = scope.clone();
      tokio::join!(
        async { run_async(s1.wait_closed(), ()).await.expect("w1") },
        async { run_async(s2.wait_closed(), ()).await.expect("w2") },
        async {
          tokio::task::yield_now().await;
          assert!(scope.close());
        },
      );
    }

    #[tokio::test]
    async fn scope_close_idempotent_second_call_noop() {
      let scope = Scope::make();
      assert!(scope.close());
      assert!(!scope.close());
      assert!(
        run_async(scope.close_signalled(), ())
          .await
          .expect("signalled")
      );
      run_async(scope.wait_closed(), ())
        .await
        .expect("wait after close");
    }

    #[tokio::test]
    async fn scope_open_latch_accessible_after_close() {
      let scope = Scope::make();
      assert!(
        !run_async(scope.close_signalled(), ())
          .await
          .expect("before")
      );
      assert!(scope.close());
      assert!(run_async(scope.close_signalled(), ()).await.expect("after"));
    }

    #[test]
    fn close_when_called_twice_is_idempotent() {
      let scope = Scope::make();
      assert!(scope.close());
      assert!(!scope.close());
      assert!(scope.is_closed());
    }

    #[test]
    fn close_with_exit_when_called_twice_is_idempotent() {
      let scope = Scope::make();
      assert!(scope.close_with_exit(Exit::succeed(())));
      assert!(!scope.close_with_exit(Exit::succeed(())));
      assert!(scope.is_closed());
    }
  }

  mod finalizers {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn add_finalizer_after_close_returns_false() {
      let scope = Scope::make();
      scope.close();
      assert!(!scope.add_finalizer(Box::new(move |_exit| {
        crate::succeed::<(), Never, ()>(())
      })));
    }

    #[test]
    fn scope_finalizers_run_in_lifo_order() {
      let scope = Scope::make();
      let calls = Arc::new(Mutex::new(Vec::new()));
      for idx in [1u8, 2u8, 3u8] {
        let calls = calls.clone();
        let ok = scope.add_finalizer(Box::new(move |_exit| {
          calls.lock().expect("calls mutex poisoned").push(idx);
          crate::succeed::<(), Never, ()>(())
        }));
        assert!(ok);
      }

      assert!(scope.close());
      let calls = calls.lock().expect("calls mutex poisoned").clone();
      assert_eq!(calls, vec![3, 2, 1]);
    }

    fn concurrent_finalizer_add_stress() {
      let scope = Scope::make();
      let n = 32usize;
      let ran = Arc::new(AtomicUsize::new(0));
      let mut handles = Vec::with_capacity(n);
      for _ in 0..n {
        let s = scope.clone();
        let ran = ran.clone();
        handles.push(std::thread::spawn(move || {
          assert!(s.add_finalizer(Box::new(move |_exit| {
            ran.fetch_add(1, Ordering::SeqCst);
            crate::succeed::<(), Never, ()>(())
          })));
        }));
      }
      for h in handles {
        h.join().expect("join");
      }
      assert!(scope.close());
      assert_eq!(ran.load(Ordering::SeqCst), n);
    }

    #[test]
    fn scope_stm_finalizers_atomic_under_concurrent_add() {
      concurrent_finalizer_add_stress();
    }

    #[test]
    fn scope_add_finalizer_under_concurrent_writers_no_data_race() {
      concurrent_finalizer_add_stress();
    }

    #[rstest]
    #[case::success(Exit::succeed(()), "success")]
    #[case::failure(Exit::interrupt(crate::runtime::FiberId::fresh()), "failure")]
    fn close_with_exit_passes_exit_variant_to_finalizers(
      #[case] exit: Exit<(), Never>,
      #[case] expected_label: &str,
    ) {
      let scope = Scope::make();
      let seen = Arc::new(Mutex::new(String::new()));
      let seen_ref = seen.clone();
      assert!(scope.add_finalizer(Box::new(move |observed_exit| {
        let label = match observed_exit {
          Exit::Success(_) => "success",
          Exit::Failure(_) => "failure",
        };
        *seen_ref.lock().expect("seen mutex poisoned") = label.to_string();
        crate::succeed::<(), Never, ()>(())
      })));

      assert!(scope.close_with_exit(exit));
      let seen = seen.lock().expect("seen mutex poisoned").clone();
      assert_eq!(seen, expected_label);
    }
  }
}
