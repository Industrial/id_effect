//! The process API driven from inside a **current-thread** tokio runtime.
//!
//! Regression guard for the `run_blocking` audit (see the `node` module
//! docs): the synchronous entry points must not wait on other processes,
//! and the effect-returning ones must be drivable with `run_async` on the
//! runtime's only thread without deadlocking.

mod common;

use std::sync::Mutex;
use std::time::Duration;

use common::{RecCall, Recorder, T, next_start, worker_args, worker_spec};
use id_effect::run_async;
use id_effect_process::{ExitReason, NodeName, Pid, ProcessNode, Strategy, SupervisorSpec};
use tokio::sync::oneshot;

fn node(name: &str) -> ProcessNode {
  ProcessNode::threads(NodeName::new(name))
}

/// Exit reason of `pid`, delivered through a monitor into an async channel.
fn exit_of(node: &ProcessNode, pid: &Pid) -> oneshot::Receiver<ExitReason> {
  let (tx, rx) = oneshot::channel();
  let tx = Mutex::new(Some(tx));
  node.monitor_with_sink(
    pid,
    Box::new(move |_m, _p, reason| {
      if let Some(tx) = tx.lock().unwrap().take() {
        let _ = tx.send(reason);
      }
    }),
  );
  rx
}

async fn within<F: std::future::Future>(fut: F) -> F::Output {
  tokio::time::timeout(T, fut)
    .await
    .expect("completed before deadline")
}

#[tokio::test(flavor = "current_thread")]
async fn call_cast_and_send_from_a_current_thread_runtime() {
  let node = node("ct-msg@test");
  let addr = node.spawn::<Recorder>(None, ());
  within(run_async(addr.cast(1), ())).await.unwrap();
  assert!(addr.send_now(2));
  within(run_async(addr.send(3), ())).await.unwrap();
  let log = within(run_async(addr.call(RecCall::Log, T), ())).await;
  assert_eq!(log, Ok(vec![1, 2, 3]));
}

#[tokio::test(flavor = "current_thread")]
async fn exit_signals_and_monitors_from_a_current_thread_runtime() {
  let node = node("ct-exit@test");
  let addr = node.spawn::<Recorder>(None, ());
  let down = exit_of(&node, addr.pid());
  node.exit(addr.pid(), ExitReason::Killed);
  assert_eq!(within(down).await, Ok(ExitReason::Killed));
  assert!(!addr.is_alive());
}

#[tokio::test(flavor = "current_thread")]
async fn call_timeout_is_measured_without_blocking_the_runtime() {
  let node = node("ct-timeout@test");
  let addr = node.spawn::<Recorder>(None, ());
  // A crash reply surfaces as Down; a zero timeout on a live call is a
  // Timeout or (if the reply wins the race) a value — never a hang.
  let out = within(run_async(addr.call(RecCall::Log, Duration::ZERO), ())).await;
  assert!(
    matches!(out, Ok(_) | Err(id_effect_process::CallError::Timeout)),
    "{out:?}"
  );
  let crash = within(run_async(addr.call(RecCall::Crash, T), ())).await;
  assert!(
    matches!(crash, Err(id_effect_process::CallError::Down(_))),
    "{crash:?}"
  );
}

#[tokio::test(flavor = "current_thread")]
async fn supervisor_restarts_are_observable_from_a_current_thread_runtime() {
  let node = node("ct-sup@test");
  let (tx, rx) = std::sync::mpsc::channel();
  let tree = SupervisorSpec::new(Strategy::OneForOne)
    .intensity(5, Duration::from_secs(60))
    .child(worker_spec(worker_args("w", &tx)))
    .start(&node);
  let (_, first) = next_start(&rx);
  let down = exit_of(&node, &first);
  node.exit(&first, ExitReason::Error("boom".to_string()));
  within(down).await.unwrap();
  let (_, second) = next_start(&rx);
  assert_ne!(first, second);
  let kids = within(run_async(tree.which_children(T), ())).await.unwrap();
  assert_eq!(kids, vec![("w".to_string(), Some(second))]);
}
