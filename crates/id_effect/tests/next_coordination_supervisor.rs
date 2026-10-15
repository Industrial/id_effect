//! `c-coordination` tests for `id_effect::next::{supervised, Supervisor, RestartIntensity}`: the
//! v1 `concurrency::supervisor` unit tests ported to the next kernel. The v1 cancellation token
//! is replaced by fiber interruption (closing the supervisor's scope), and the v1 `Schedule` by a
//! fixed restart delay.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use id_effect::next::{
  Effect, ExitKind, RestartIntensity, Scope, Supervisor, SupervisorPolicy, TestExecutor, die, fail,
  succeed, supervised,
};
use id_effect::runtime::Never;
use id_effect::{Cause, Exit};

fn run_exit<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  eff.block_on()
}

fn counter() -> Arc<AtomicUsize> {
  Arc::new(AtomicUsize::new(0))
}

/// `make` for a child that fails `fails` times with `"e"`, then succeeds with 42.
fn flaky(
  n: &Arc<AtomicUsize>,
  fails: usize,
) -> impl FnMut() -> Effect<u8, &'static str> + Send + use<> {
  let n = Arc::clone(n);
  move || {
    if n.fetch_add(1, Ordering::SeqCst) < fails {
      fail("e")
    } else {
      succeed(42)
    }
  }
}

mod restart_intensity {
  use super::*;

  #[test]
  fn note_reports_over_budget_within_window() {
    let mut intensity = RestartIntensity::new(2, Duration::from_secs(1));
    let t0 = Instant::now();
    assert!(intensity.note(t0), "1st restart within budget");
    assert!(intensity.note(t0), "2nd restart within budget");
    assert!(!intensity.note(t0), "3rd restart exceeds max_restarts=2");
  }

  #[test]
  fn old_events_slide_out_of_window() {
    let mut intensity = RestartIntensity::new(1, Duration::from_millis(100));
    let t0 = Instant::now();
    assert!(intensity.note(t0));
    assert!(!intensity.note(t0 + Duration::from_millis(50)));
    assert!(
      intensity.note(t0 + Duration::from_millis(500)),
      "earlier events slid out once the window elapsed"
    );
  }

  #[test]
  fn accessors_report_configuration() {
    let intensity = RestartIntensity::new(3, Duration::from_secs(5));
    assert_eq!(intensity.max_restarts(), 3);
    assert_eq!(intensity.within(), Duration::from_secs(5));
  }
}

mod supervisor {
  use super::*;

  #[test]
  fn closing_parent_scope_shuts_the_supervisor_down() {
    let parent = Scope::new();
    let sup = Supervisor::new();
    let exec = TestExecutor::new();
    let calls = counter();
    let c = Arc::clone(&calls);
    let setup = sup.attach_to::<Never, _>(&parent).then(sup.spawn(
      SupervisorPolicy::Restart {
        delay: Duration::from_secs(1),
      },
      move || {
        c.fetch_add(1, Ordering::SeqCst);
        fail::<(), &'static str, _>("again")
      },
    ));
    let h = setup.fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    assert!(h.is_done());
    let Exit::Success(child) = take(h) else {
      panic!("setup failed")
    };
    assert!(!sup.is_shutdown());
    assert!(
      !child.is_done(),
      "the restart loop is sleeping between attempts"
    );
    let closer = parent
      .close(ExitKind::Success)
      .fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    assert!(closer.is_done());
    assert!(sup.is_shutdown());
    assert!(
      child.is_done(),
      "closing the parent interrupted the supervised fiber"
    );
    assert!(matches!(take(child), Exit::Failure(Cause::Interrupt(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn shutdown_interrupts_a_supervised_loop() {
    let sup = Supervisor::new();
    let calls = counter();
    let c = Arc::clone(&calls);
    let s2 = sup.clone();
    let eff = sup
      .spawn(
        SupervisorPolicy::Restart {
          delay: Duration::ZERO,
        },
        move || {
          c.fetch_add(1, Ordering::SeqCst);
          fail::<(), &'static str, _>("again")
        },
      )
      .flat_map(move |h| {
        id_effect::next::sleep(Duration::from_millis(20))
          .then(s2.shutdown())
          .then(h.join().exit())
      });
    let Exit::Success(out) = run_exit(eff) else {
      panic!("root failed")
    };
    assert!(matches!(out, Exit::Failure(Cause::Interrupt(_))), "{out:?}");
    assert!(calls.load(Ordering::SeqCst) >= 1);
    assert!(sup.is_shutdown());
    // Forking into a shut-down supervisor yields a fiber interrupted before it runs.
    let late = run_exit(
      sup
        .fork(succeed::<u8, Never, _>(1))
        .flat_map(|h| h.join().exit::<Never>()),
    );
    assert!(matches!(
      late,
      Exit::Success(Exit::Failure(Cause::Interrupt(_)))
    ));
  }

  fn take<A: Send + 'static, E: Send + 'static>(
    h: id_effect::next::FiberHandle<A, E>,
  ) -> Exit<A, E> {
    use std::future::Future;
    let mut fut = std::pin::pin!(h.join_future());
    match fut
      .as_mut()
      .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
      std::task::Poll::Ready(v) => v,
      std::task::Poll::Pending => panic!("fiber not done"),
    }
  }
}

mod supervised_policy {
  use super::*;

  #[test]
  fn terminate_returns_ok_on_first_success() {
    let out = run_exit(supervised(SupervisorPolicy::Terminate, || {
      succeed::<u8, &'static str, _>(7)
    }));
    assert!(matches!(out, Exit::Success(7)));
  }

  #[test]
  fn terminate_returns_fail_cause_on_first_error_without_retry() {
    let n = counter();
    let out = run_exit(supervised(SupervisorPolicy::Terminate, flaky(&n, 10)));
    assert!(matches!(out, Exit::Failure(Cause::Fail("e"))));
    assert_eq!(n.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn escalate_fails_fast() {
    let n = counter();
    let out = run_exit(supervised(SupervisorPolicy::Escalate, flaky(&n, 10)));
    assert!(matches!(out, Exit::Failure(Cause::Fail("e"))));
    assert_eq!(n.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn ignore_recovers_with_the_given_value() {
    let n = counter();
    let out = run_exit(supervised(
      SupervisorPolicy::Ignore { recover: 9 },
      flaky(&n, 10),
    ));
    assert!(matches!(out, Exit::Success(9)));
  }

  #[test]
  fn restart_retries_until_child_succeeds_with_zero_delay() {
    let n = counter();
    let out = run_exit(supervised(
      SupervisorPolicy::Restart {
        delay: Duration::ZERO,
      },
      flaky(&n, 2),
    ));
    assert!(matches!(out, Exit::Success(42)));
    assert_eq!(n.load(Ordering::SeqCst), 3);
  }

  #[test]
  fn restart_waits_the_delay_between_attempts_on_virtual_time() {
    let n = counter();
    let exec = TestExecutor::new();
    let h = supervised(
      SupervisorPolicy::Restart {
        delay: Duration::from_secs(60),
      },
      flaky(&n, 2),
    )
    .fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    exec.advance(Duration::from_secs(60));
    assert_eq!(n.load(Ordering::SeqCst), 2);
    exec.advance(Duration::from_secs(60));
    assert!(h.is_done());
    assert_eq!(n.load(Ordering::SeqCst), 3);
  }

  #[test]
  fn restart_restarts_defects_too() {
    let n = counter();
    let n2 = Arc::clone(&n);
    let out = run_exit(supervised(
      SupervisorPolicy::Restart {
        delay: Duration::ZERO,
      },
      move || {
        if n2.fetch_add(1, Ordering::SeqCst) == 0 {
          die::<u8, Never, _>("crash")
        } else {
          succeed(1)
        }
      },
    ));
    assert!(matches!(out, Exit::Success(1)));
    assert_eq!(n.load(Ordering::SeqCst), 2);
  }

  #[test]
  fn restart_with_limit_zero_escalates_on_the_first_failure() {
    let n = counter();
    let out = run_exit(supervised(
      SupervisorPolicy::RestartWithLimit {
        limit: 0,
        delay: Duration::ZERO,
      },
      flaky(&n, 10),
    ));
    assert!(matches!(out, Exit::Failure(Cause::Fail("e"))));
    assert_eq!(n.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn restart_with_limit_escalates_with_then_aggregated_cause() {
    let n = counter();
    let out = run_exit(supervised(
      SupervisorPolicy::RestartWithLimit {
        limit: 1,
        delay: Duration::ZERO,
      },
      flaky(&n, 10),
    ));
    match out {
      Exit::Failure(Cause::Then(a, b)) => {
        assert!(matches!(*a, Cause::Fail("e")));
        assert!(matches!(*b, Cause::Fail("e")));
      }
      other => panic!("unexpected exit: {other:?}"),
    }
    assert_eq!(n.load(Ordering::SeqCst), 2);
  }

  #[test]
  fn tight_restart_loop_stays_bounded_under_high_limit() {
    let n = counter();
    let out = run_exit(supervised(
      SupervisorPolicy::RestartWithLimit {
        limit: 50,
        delay: Duration::ZERO,
      },
      flaky(&n, 10),
    ));
    assert!(matches!(out, Exit::Success(42)));
    assert_eq!(n.load(Ordering::SeqCst), 11);
  }

  #[test]
  fn interrupt_is_never_restarted() {
    let n = counter();
    let n2 = Arc::clone(&n);
    let exec = TestExecutor::new();
    let h = supervised(
      SupervisorPolicy::Restart {
        delay: Duration::ZERO,
      },
      move || {
        n2.fetch_add(1, Ordering::SeqCst);
        id_effect::next::sleep::<&'static str, _>(Duration::from_secs(3600)).map(|()| 0u8)
      },
    )
    .fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    h.interrupt_now();
    exec.run_until_stalled();
    assert!(h.is_done());
    assert_eq!(
      n.load(Ordering::SeqCst),
      1,
      "no restart after the interrupt"
    );
  }
}
