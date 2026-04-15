//! Ex 048 — `run_fork` on [`ThreadSleepRuntime`] runs the effect on a worker thread; [`FiberHandle::join`]
//! returns the success value.
use id_effect::{ThreadSleepRuntime, run_fork, succeed};

fn main() {
  let rt = ThreadSleepRuntime;
  let h = run_fork(&rt, || (succeed::<u8, (), ()>(5_u8), ()));
  assert_eq!(pollster::block_on(h.join()), Ok(5));
  println!("048_run_fork_join ok");
}
