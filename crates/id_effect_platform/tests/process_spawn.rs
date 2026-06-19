use id_effect::{build_env, provide, run_async};
use id_effect_platform::process::{CommandSpec, TokioProcessRuntimeProvider, spawn_wait};

#[cfg(unix)]
#[tokio::test]
async fn spawn_true_exits_zero() {
  let env = build_env([provide!(TokioProcessRuntimeProvider)]).expect("providers");
  let st = run_async(spawn_wait(CommandSpec::new("true")), env)
    .await
    .expect("spawn");
  assert!(st.success());
}
