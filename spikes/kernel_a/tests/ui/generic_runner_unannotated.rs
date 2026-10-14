// (A-specific, risk R3(b)) A runner generic over a *subset* environment: the inline closure's
// `R` cannot be inferred (`~Counter` constrains it but does not determine it). Fix: annotate the
// closure, `effect!(|_r: &caps!(Counter)| ..)`, or tie `R` to the env as corpus H05 does.
use kernel_a::{Effect, Embed, HList, Never, effect, hlist};
use spike_corpus::Counter;

fn execute<Env, R, Is, F>(env: Env, f: F) -> String
where
  Env: Embed<R, Is> + Send + 'static,
  R: HList,
  F: FnOnce() -> Effect<String, Never, R>,
{
  f().provide_env(env).block_on().unwrap_or_default()
}

fn main() {
  let _ = execute(hlist![Counter(7)], || {
    effect!({ format!("n={}", ~Counter.0) })
  });
}
