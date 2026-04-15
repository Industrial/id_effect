//! Ex 032 — `service_env` is a one-cell `Context`.
use id_effect::{Get, ServiceEnv, run_blocking, service_env, service_key, succeed};

service_key!(struct TokenKey);

fn main() {
  let env = service_env::<TokenKey, _>("secret");
  assert_eq!(*Get::<TokenKey>::get(&env), "secret");
  assert_eq!(
    run_blocking(succeed::<(), (), ServiceEnv<TokenKey, &str>>(()), env),
    Ok(())
  );
  println!("032_service_env ok");
}
