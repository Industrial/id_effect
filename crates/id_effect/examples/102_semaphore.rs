//! Ex 102 — `Semaphore` / `Permit` gate concurrent async work.
use id_effect::{Semaphore, run_async, run_blocking};

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let sem = run_blocking(Semaphore::make(1), ()).expect("sem");
  let p = run_async(sem.try_acquire(), ())
    .await
    .expect("try")
    .expect("permit");
  drop(p);
  println!("102_semaphore ok");
}
