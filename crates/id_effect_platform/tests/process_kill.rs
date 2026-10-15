#![cfg(unix)]

mod common;

use common::run;
use id_effect::HNil;
use id_effect_platform::command::{
  CommandSpec, TokioProcessRuntime, child_kill, child_wait, process_runtime_env, spawn,
};

#[tokio::test]
async fn spawn_kill_terminates_long_running_child() {
  let env = process_runtime_env(TokioProcessRuntime);
  let handle = run(spawn(CommandSpec::new("sleep").arg("60")).provide_all(env))
    .await
    .expect("spawn");
  run(child_kill::<HNil>(handle.clone())).await.expect("kill");
  let status = run(child_wait::<HNil>(handle))
    .await
    .expect("wait after kill");
  assert!(!status.success());
}
