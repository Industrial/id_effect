// (H17 residue, risk R3(b)) A runner generic over a *subset* environment: the inline closure's
// `R` cannot be inferred (`~Counter` constrains it but does not determine it). Fix: annotate the
// closure, `effect_next!(|_r: &caps_next!(Counter)| ..)`, or tie `R` to the env as corpus H05
// does.
use id_effect::next::{Effect, Embed, HList, effect_next, hlist};
use id_effect::{Exit, Never};

pub struct Counter(pub u32);

fn execute<Env, R, Is, F>(env: Env, f: F) -> String
where
  Env: Embed<R, Is> + Send + Sync + 'static,
  R: HList,
  F: FnOnce() -> Effect<String, Never, R>,
{
  match f().provide_env(env).block_on() {
    Exit::Success(s) => s,
    Exit::Failure(_) => String::new(),
  }
}

fn main() {
  let _ = execute(hlist![Counter(7)], || {
    effect_next!({ format!("n={}", ~Counter.0) })
  });
}
