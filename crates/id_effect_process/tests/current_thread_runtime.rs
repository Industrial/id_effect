//! The process API driven from inside a **current-thread** tokio runtime.
//!
//! The node runs its process fibers on that runtime's only thread
//! (`TokioExecutor::current()`): the synchronous entry points must not wait
//! on other processes, and the effect-returning ones must be drivable with
//! `Runtime::run(..).await` on the same thread without deadlocking.

mod common;

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{RecCall, Recorder, T, worker_args, worker_spec};
use id_effect::Exit;
use id_effect::next::{Effect, HNil, TokioExecutor};
use id_effect_process::{ExitReason, NodeName, Pid, ProcessNode, Strategy, SupervisorSpec};
use tokio::sync::oneshot;

fn node(name: &str) -> ProcessNode {
  let exec = TokioExecutor::current().expect("inside a tokio runtime");
  ProcessNode::on_executor(NodeName::new(name), Arc::new(exec))
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

/// Run `eff` on the node's runtime from this (the runtime's only) thread.
async fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(
  node: &ProcessNode,
  eff: Effect<A, E, HNil>,
) -> Result<A, E> {
  match within(node.runtime().run(eff)).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(id_effect::Cause::Fail(e)) => Err(e),
    Exit::Failure(cause) => panic!("effect died: {cause:?}"),
  }
}

/// Next start notification, yielding to the runtime until it arrives.
async fn next_start(rx: &Receiver<(&'static str, Pid)>) -> (&'static str, Pid) {
  within(async {
    loop {
      if let Ok(start) = rx.try_recv() {
        return start;
      }
      tokio::task::yield_now().await;
    }
  })
  .await
}

#[tokio::test(flavor = "current_thread")]
async fn call_cast_and_send_from_a_current_thread_runtime() {
  let node = node("ct-msg@test");
  let addr = node.spawn::<Recorder>(None, ());
  run(&node, addr.cast(1)).await.unwrap();
  assert!(addr.send_now(2));
  run(&node, addr.send(3)).await.unwrap();
  let log = run(&node, addr.call(RecCall::Log, T)).await;
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
  // A zero timeout on a live call is a Timeout or (if the reply wins the
  // race) a value — never a hang.
  let out = run(&node, addr.call(RecCall::Log, Duration::ZERO)).await;
  assert!(
    matches!(out, Ok(_) | Err(id_effect_process::CallError::Timeout)),
    "{out:?}"
  );
  let crash = run(&node, addr.call(RecCall::Crash, T)).await;
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
  let (_, first) = next_start(&rx).await;
  let down = exit_of(&node, &first);
  node.exit(&first, ExitReason::Error("boom".to_string()));
  within(down).await.unwrap();
  let (_, second) = next_start(&rx).await;
  assert_ne!(first, second);
  let kids = run(&node, tree.which_children(T)).await.unwrap();
  assert_eq!(kids, vec![("w".to_string(), Some(second))]);
}
