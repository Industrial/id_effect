// Gallery: a runner generic over a subset environment, called with an unannotated closure; `R`
// cannot be inferred. Fix: annotate the closure's effect, `|| -> Effect<_, _, caps!(Counter)>`.
use id_effect::{Effect, Embed, HList, effect, hlist};
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
    effect!({ format!("n={}", ~Counter.0) })
  });
}
