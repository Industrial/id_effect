// Gallery: `Effect::retry(..)` given an effect instead of a factory that makes one per attempt.
// Fix: pass the constructor, `Effect::retry(fetch, schedule)` or `Effect::retry(|| fetch(), ..)`.
use id_effect::{Effect, Schedule, fail};

pub struct HttpError;

fn fetch() -> Effect<String, HttpError> {
  fail(HttpError)
}

fn main() {
  let _ = Effect::retry(fetch(), Schedule::recurs(3));
}
