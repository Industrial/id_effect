//! A traffic light as a supervised `gen_statem`-style process.
//!
//! The `Amber -> Red` action fails once (a flaky bulb driver). The failure crashes the
//! `FsmProcess`; its `OneForOne` supervisor restarts it from the spec, back at `Red`.
//!
//! ```bash
//! cargo run -p id_effect_fsm --example supervised_traffic_light
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{Effect, run_blocking};
use id_effect_fsm::{Fsm, FsmHandle, FsmProcess, FsmSpec, Interpreter};
use id_effect_process::{
  CallError, ChildSpec, NodeName, ProcessNode, StartFn, Strategy, SupervisorSpec,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Tick {
  Next,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
#[fsm(event = Tick, initial = Red)]
enum Light {
  #[fsm(on(Next => Green))]
  Red,
  #[fsm(on(Next => Amber))]
  Green,
  #[fsm(on(Next => Red))]
  Amber,
}

type Handle = FsmHandle<Light, Tick, String, ()>;

fn main() -> Result<(), CallError> {
  let timeout = Duration::from_secs(5);
  let flaky = Arc::new(AtomicBool::new(true));
  let actions = Interpreter::new().on_transition(Light::Amber, Tick::Next, move || {
    let flaky = Arc::clone(&flaky);
    Effect::new(move |_| {
      if flaky.swap(false, Ordering::SeqCst) {
        Err("bulb driver timeout".to_string())
      } else {
        Ok(())
      }
    })
  });
  let spec: FsmSpec<Light, Tick, String, ()> = FsmSpec::from_fsm().with_interpreter(actions);

  // The start closure publishes each incarnation's handle so `main` can talk to it.
  let current: Arc<Mutex<Option<Handle>>> = Arc::default();
  let publish = Arc::clone(&current);
  let start: StartFn = Arc::new(move |node, supervisor| {
    let addr =
      node.spawn_linked_to::<FsmProcess<Light, Tick, String, ()>>(supervisor, spec.clone(), ());
    let pid = addr.pid().clone();
    *publish.lock().unwrap() = Some(FsmHandle::from_addr(addr));
    Ok(pid)
  });

  let node = ProcessNode::threads(NodeName::new("traffic@local"));
  let tree = SupervisorSpec::new(Strategy::OneForOne)
    .child(ChildSpec::from_start("light", start))
    .start(&node);
  let handle = || current.lock().unwrap().clone().expect("light started");
  run_blocking(tree.which_children(timeout), ())?; // start closure has run

  let light = handle();
  println!("{:?}", run_blocking(light.fire(Tick::Next, timeout), ())?); // Ok(Green)
  println!("{:?}", run_blocking(light.fire(Tick::Next, timeout), ())?); // Ok(Amber)
  // Amber -> Red runs the flaky action: the process crashes, the call reports Down.
  println!("{:?}", run_blocking(light.fire(Tick::Next, timeout), ()));

  // The supervisor restarted the light from its spec.
  let children = run_blocking(tree.which_children(timeout), ())?;
  println!("children after restart: {children:?}");
  let light = handle();
  println!(
    "restarted light is {:?}",
    run_blocking(light.state(timeout), ())?
  );
  Ok(())
}
