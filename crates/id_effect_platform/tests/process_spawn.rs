#![cfg(unix)]

mod common;

use common::run;
use id_effect_platform::command::{
  CommandSpec, ProcessRuntimeEnv, TokioProcessRuntimeProvider, spawn_wait,
};

#[tokio::test]
async fn spawn_true_exits_zero() {
  let program = spawn_wait::<ProcessRuntimeEnv, _>(CommandSpec::new("true"))
    .provide_layer(TokioProcessRuntimeProvider::layer());
  let st = run(program).await.expect("spawn");
  assert!(st.success());
}

#[tokio::test]
async fn spawn_false_exits_non_zero() {
  let program = spawn_wait::<ProcessRuntimeEnv, _>(CommandSpec::new("false"))
    .provide_layer(TokioProcessRuntimeProvider::layer());
  let st = run(program).await.expect("spawn");
  assert!(!st.success());
}

#[tokio::test]
async fn spawn_missing_program_is_a_typed_error() {
  let program =
    spawn_wait::<ProcessRuntimeEnv, _>(CommandSpec::new("/nonexistent/id-effect-platform-probe"))
      .provide_layer(TokioProcessRuntimeProvider::layer());
  assert!(run(program).await.is_err());
}
