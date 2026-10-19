//! A code lock as a `Statem` (the `code_lock` example of Erlang's `gen_statem` guide).
//!
//! - The state carries data (`Locked { failed }`); the machine `Data` holds the code and the
//!   buttons pressed so far.
//! - Entering `Locked` clears the buttons; entering `Open` arms a state timeout that locks the
//!   door again.
//! - Buttons pressed while `Open` are postponed and re-delivered once the door has locked.
//! - An event timeout clears a half-typed code after a pause without presses.
//! - `Status` is a call, answered with `Action::Reply`.
//!
//! ```bash
//! cargo run -p id_effect_fsm --example statem_door_lock
//! ```

use std::thread::sleep;
use std::time::Duration;

use id_effect::{Cause, Exit};
use id_effect::{Effect, HNil, succeed};
use id_effect_fsm::{
  Action, Entered, Statem, StatemEvent, StatemHandle, StatemInit, StatemProcess, Transition,
};
use id_effect_process::{CallError, NodeName, ProcessCtx, ProcessNode};

const OPEN_FOR: Duration = Duration::from_millis(200);
const PAUSE: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq)]
enum Door {
  Locked { failed: u32 },
  Open,
}

#[derive(Clone, Debug, PartialEq)]
enum Input {
  Button(u8),
  /// State-timeout payload: lock the open door.
  Lock,
  /// Event-timeout payload: forget a half-typed code.
  Forget,
  /// Call: describe the lock.
  Status,
}

struct Keypad {
  code: Vec<u8>,
  buttons: Vec<u8>,
}

struct CodeLock;

impl Statem for CodeLock {
  type Args = Vec<u8>;
  type Env = ();
  type State = Door;
  type Data = Keypad;
  type Event = Input;
  type Reply = String;
  type Error = String;

  fn init(code: Vec<u8>, _ctx: ProcessCtx) -> Effect<StatemInit<Self>, String, ()> {
    let keypad = Keypad {
      code,
      buttons: Vec::new(),
    };
    succeed((Door::Locked { failed: 0 }, keypad, Vec::new()))
  }

  fn handle_event(
    event: &StatemEvent<Self>,
    door: &Door,
    mut keypad: Keypad,
    _ctx: ProcessCtx,
  ) -> Effect<Transition<Self>, String, ()> {
    let transition = match (event, door) {
      (StatemEvent::Call(Input::Status, to), _) => {
        let status = format!("{door:?}, {} button(s) pressed", keypad.buttons.len());
        Transition::Keep(keypad, vec![Action::Reply(to.clone(), status)])
      }
      (StatemEvent::Cast(Input::Button(_)), Door::Open) => {
        Transition::Keep(keypad, vec![Action::Postpone])
      }
      (StatemEvent::Cast(Input::Button(b)), Door::Locked { failed }) => {
        keypad.buttons.push(*b);
        if keypad.buttons.len() < keypad.code.len() {
          Transition::Keep(keypad, vec![Action::EventTimeout(PAUSE, Input::Forget)])
        } else if keypad.buttons == keypad.code {
          Transition::Next(Door::Open, keypad, Vec::new())
        } else {
          let failed = failed + 1;
          Transition::Next(Door::Locked { failed }, keypad, Vec::new())
        }
      }
      (StatemEvent::EventTimeout(Input::Forget), _) => {
        println!("  pause: clearing {:?}", keypad.buttons);
        keypad.buttons.clear();
        Transition::Keep(keypad, Vec::new())
      }
      (StatemEvent::StateTimeout(Input::Lock), Door::Open) => {
        Transition::Next(Door::Locked { failed: 0 }, keypad, Vec::new())
      }
      _ => Transition::Keep(keypad, Vec::new()),
    };
    succeed(transition)
  }

  fn on_enter(
    _from: Option<&Door>,
    door: &Door,
    mut keypad: Keypad,
    _ctx: ProcessCtx,
  ) -> Effect<Entered<Self>, String, ()> {
    println!("  enter {door:?}");
    keypad.buttons.clear();
    let actions = match door {
      Door::Open => vec![Action::StateTimeout(OPEN_FOR, Input::Lock)],
      Door::Locked { .. } => Vec::new(),
    };
    succeed((keypad, actions))
  }
}

/// Run an effect (the process API) at the edge.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E, HNil>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died"),
  }
}

fn press(lock: &StatemHandle<CodeLock>, buttons: &[u8]) {
  for &b in buttons {
    run(lock.cast(Input::Button(b))).expect("lock is running");
  }
}

fn main() -> Result<(), CallError> {
  let timeout = Duration::from_secs(5);
  let node = ProcessNode::new(NodeName::new("door@local"));
  let lock = StatemProcess::<CodeLock>::start(&node, vec![1, 2, 3, 4], ());
  let status = || run(lock.call(Input::Status, timeout));

  println!("wrong code:");
  press(&lock, &[1, 2, 3, 9]);
  println!("{}", status()?); // Locked { failed: 1 }, 0 button(s) pressed

  println!("right code, then two presses while open (postponed):");
  press(&lock, &[1, 2, 3, 4]);
  press(&lock, &[4, 3]);
  println!("{}", status()?); // Open, 0 button(s) pressed

  // The state timeout locks the door; the postponed presses are re-delivered in `Locked`.
  sleep(OPEN_FOR + PAUSE / 2);
  println!("{}", status()?); // Locked { failed: 0 }, 2 button(s) pressed

  // One more press makes three of four; the pause after it clears them (event timeout).
  println!("one more press, then a pause:");
  press(&lock, &[1]);
  sleep(PAUSE * 2);
  println!("{}", status()?); // Locked { failed: 0 }, 0 button(s) pressed
  Ok(())
}
