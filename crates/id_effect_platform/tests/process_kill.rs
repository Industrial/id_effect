#[cfg(unix)]
use id_effect::{build_env, provide, run_async};
use id_effect_platform::process::{
  CommandSpec, TokioProcessRuntimeProvider, child_kill, child_wait, spawn,
};

#[cfg(unix)]
#[tokio::test]
async fn spawn_kill_terminates_long_running_child() {
  let env = build_env([provide!(TokioProcessRuntimeProvider)]).expect("providers");
  let handle = run_async(spawn(CommandSpec::new("sleep").arg("60")), env.clone())
    .await
    .expect("spawn");
  run_async(child_kill(handle.clone()), env.clone())
    .await
    .expect("kill");
  let status = run_async(child_wait(handle), env)
    .await
    .expect("wait after kill");
  assert!(!status.success());
}
