//! Ex 087 — `TSet`: membership changes commit atomically with other cells (book:
//! "Transactional Collections").
//!
//! Run: `cargo run -p id_effect --example 087_stm_tset`
use id_effect::Effect;
use id_effect::stm::{Stm, TRef, TSet, commit};
use id_effect::{Cause, Exit};

fn main() {
  let online = run(commit(TSet::<String>::make())).expect("set");
  let logins = run(commit(TRef::make(0_u32))).expect("cell");

  // Log in: add to the set and count the login only when the user was not already online.
  let login = |user: &str| {
    let logins = logins.clone();
    online
      .add::<()>(user.to_string())
      .flat_map(move |fresh| logins.update_stm(move |n| if fresh { n + 1 } else { n }))
  };
  run(commit(login("alice"))).expect("login");
  run(commit(login("alice"))).expect("login again");
  run(commit(login("bob"))).expect("login");
  assert_eq!(run(commit(online.size::<()>())), Ok(2));
  assert_eq!(run(commit(logins.read_stm::<()>())), Ok(2));

  // A failing transaction rolls back the removal.
  let failing = online
    .remove::<&str>(&"bob".to_string())
    .flat_map(|_| Stm::<(), &str>::fail("audit log unavailable"));
  assert_eq!(run(commit(failing)), Err("audit log unavailable"));
  assert_eq!(
    run(commit(online.contains::<()>(&"bob".to_string()))),
    Ok(true)
  );

  // `take_any` retries while empty; `or_else` turns that into a non-blocking poll.
  let poll = online
    .take_any::<()>()
    .map(Some)
    .or_else(Stm::succeed(None));
  let mut drained = Vec::new();
  while let Ok(Some(user)) = run(commit(poll.clone())) {
    drained.push(user);
  }
  drained.sort();
  assert_eq!(drained, ["alice", "bob"]);
  println!("087_stm_tset ok");
}

/// Runs `eff` on the global runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
