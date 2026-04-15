// Test: A-01 no async fn
// This should trigger NO_ASYNC_FN_IN_EFFECT_CRATE

async fn bad_fetch(url: &str) -> String {
  //~^ ERROR `async fn` is forbidden in Effect.rs code
  url.to_string()
}

fn main() {}
