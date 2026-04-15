//! Ex 082 — `TSemaphore` acquire/release in a transaction.
use id_effect::{TSemaphore, commit, run_blocking};

fn main() {
  let stm =
    TSemaphore::make(1).flat_map(|s| s.acquire::<()>().flat_map(move |_| s.release::<()>()));
  run_blocking(commit(stm), ()).expect("commit");
  println!("082_stm_tsemaphore ok");
}
