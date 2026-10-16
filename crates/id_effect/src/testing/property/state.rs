//! Stateful (model-based) property testing: random command sequences run against a pure model
//! and against the system under test, shrunk to a minimal failing sequence.

use std::fmt::Debug;
use std::panic::Location;

use proptest::collection::{SizeRange, vec};
use proptest::prelude::{BoxedStrategy, Strategy};
use proptest::test_runner::{Config, TestCaseError, TestError, TestRunner};

use super::prop_check_on;
use crate::Effect;
use crate::testing::TestExecutor;

/// A model of a system and the commands that drive it.
///
/// Each case starts a fresh [`TestExecutor`], builds the system with
/// [`init_sut`](Self::init_sut), and walks a generated command list: a command whose
/// [`precondition`](Self::precondition) does not hold in the current model state is skipped;
/// otherwise the model steps with [`next_state`](Self::next_state) and the system with
/// [`apply`](Self::apply), which checks the system against the model (state before and after).
/// Every effect of a case runs on the same executor, so virtual time carries over between
/// commands.
///
/// Commands are drawn independently of the state (skipping replaces state-dependent
/// generation), so proptest can shrink a failing list by dropping and simplifying commands.
pub trait StateMachine {
  /// The model state.
  type State: Clone + Debug;
  /// The system under test (often a cheap-to-clone handle such as a `Queue` or `Ref`).
  type Sut: Send + 'static;
  /// One operation.
  type Command: Clone + Debug + 'static;

  /// The model state before any command.
  fn initial_state(&self) -> Self::State;

  /// The commands to draw from.
  fn commands(&self) -> BoxedStrategy<Self::Command>;

  /// Whether `command` may run in `state` (default: always).
  fn precondition(&self, state: &Self::State, command: &Self::Command) -> bool {
    let _ = (state, command);
    true
  }

  /// The model state after `command`.
  fn next_state(&self, state: &Self::State, command: &Self::Command) -> Self::State;

  /// Builds a fresh system for one case.
  fn init_sut(&self) -> Effect<Self::Sut, TestCaseError>;

  /// Runs `command` on the system and checks the result against the model (`prop_assert!`
  /// inside [`prop_sync`](super::prop_sync), or fail with a `TestCaseError`).
  fn apply(
    &self,
    sut: &Self::Sut,
    command: &Self::Command,
    before: &Self::State,
    after: &Self::State,
  ) -> Effect<(), TestCaseError>;
}

/// Lists of `size` commands drawn from `machine`.
pub fn commands<M: StateMachine>(
  machine: &M,
  size: impl Into<SizeRange>,
) -> impl Strategy<Value = Vec<M::Command>> {
  vec(machine.commands(), size)
}

/// Runs one command list against `machine` (use inside your own `proptest!` with
/// [`commands`]). A failure names the step and the command.
pub fn check_commands<M: StateMachine>(
  machine: &M,
  commands: &[M::Command],
) -> Result<(), TestCaseError> {
  let exec = TestExecutor::new();
  let sut = prop_check_on(&exec, machine.init_sut())?;
  let mut state = machine.initial_state();
  for (step, command) in commands.iter().enumerate() {
    if !machine.precondition(&state, command) {
      continue;
    }
    let next = machine.next_state(&state, command);
    prop_check_on(&exec, machine.apply(&sut, command, &state, &next)).map_err(|e| match e {
      TestCaseError::Fail(why) => {
        TestCaseError::fail(format!("step {step} {command:?} (model {state:?}): {why}"))
      }
      reject => reject,
    })?;
    state = next;
  }
  Ok(())
}

/// Runs `config.cases` random command lists of `size` commands against `machine`, with
/// proptest shrinking and failure persistence (`proptest-regressions/` next to the calling
/// file, unless `config.source_file` is set).
///
/// # Panics
///
/// With the minimal failing command list and the reason when a case fails.
///
/// ```
/// use id_effect::testing::property::{StateMachine, prop_sync, run_state_machine};
/// use id_effect::{Effect, Ref};
/// use proptest::prelude::*;
/// use proptest::test_runner::Config;
///
/// struct Counter;
///
/// impl StateMachine for Counter {
///   type State = i64;
///   type Sut = Ref<i64>;
///   type Command = i64;
///
///   fn initial_state(&self) -> i64 { 0 }
///   fn commands(&self) -> BoxedStrategy<i64> { (-5i64..5).boxed() }
///   fn next_state(&self, s: &i64, add: &i64) -> i64 { s + add }
///   fn init_sut(&self) -> Effect<Ref<i64>, TestCaseError> { id_effect::succeed(Ref::new(0)) }
///   fn apply(&self, r: &Ref<i64>, add: &i64, _: &i64, after: &i64) -> Effect<(), TestCaseError> {
///     let (add, after) = (*add, *after);
///     r.update_and_get(move |n| n + add)
///       .flat_map(move |n| prop_sync(move || { prop_assert_eq!(n, after); Ok(()) }))
///   }
/// }
///
/// run_state_machine(&Counter, Config::with_cases(32), 0..20);
/// ```
#[track_caller]
pub fn run_state_machine<M: StateMachine>(machine: &M, config: Config, size: impl Into<SizeRange>) {
  let mut config = config;
  if config.source_file.is_none() {
    config.source_file = Some(Location::caller().file());
  }
  let mut runner = TestRunner::new(config);
  let result = runner.run(&commands(machine, size), |cmds| {
    check_commands(machine, &cmds)
  });
  match result {
    Ok(()) => {}
    Err(TestError::Fail(why, cmds)) => {
      panic!("state machine failed: {why}\nminimal failing commands: {cmds:#?}")
    }
    Err(TestError::Abort(why)) => panic!("state machine aborted: {why}"),
  }
}

#[cfg(test)]
mod tests {
  use std::collections::VecDeque;
  use std::time::Duration;

  use proptest::prelude::*;

  use super::super::prop_sync;
  use super::*;
  use crate::{Queue, QueueError, sleep, succeed};

  #[derive(Clone, Debug)]
  enum Cmd {
    Offer(u8),
    Poll,
    Wait(u64),
  }

  /// A bounded queue against a `VecDeque` model.
  struct BoundedQueue {
    cap: usize,
  }

  impl StateMachine for BoundedQueue {
    type State = VecDeque<u8>;
    type Sut = Queue<u8>;
    type Command = Cmd;

    fn initial_state(&self) -> VecDeque<u8> {
      VecDeque::new()
    }

    fn commands(&self) -> BoxedStrategy<Cmd> {
      prop_oneof![
        any::<u8>().prop_map(Cmd::Offer),
        Just(Cmd::Poll),
        (1u64..100).prop_map(Cmd::Wait),
      ]
      .boxed()
    }

    fn precondition(&self, state: &VecDeque<u8>, cmd: &Cmd) -> bool {
      !matches!(cmd, Cmd::Offer(_)) || state.len() < self.cap
    }

    fn next_state(&self, state: &VecDeque<u8>, cmd: &Cmd) -> VecDeque<u8> {
      let mut s = state.clone();
      match cmd {
        Cmd::Offer(n) => s.push_back(*n),
        Cmd::Poll => {
          s.pop_front();
        }
        Cmd::Wait(_) => {}
      }
      s
    }

    fn init_sut(&self) -> Effect<Queue<u8>, TestCaseError> {
      succeed(Queue::bounded(self.cap))
    }

    fn apply(
      &self,
      q: &Queue<u8>,
      cmd: &Cmd,
      before: &VecDeque<u8>,
      after: &VecDeque<u8>,
    ) -> Effect<(), TestCaseError> {
      let expected_front = before.front().copied();
      let expected_len = after.len();
      let q2 = q.clone();
      let step: Effect<(), TestCaseError> = match cmd {
        Cmd::Offer(n) => q.offer(*n).map(|_| ()),
        Cmd::Poll => q
          .poll()
          .map_err(|e: QueueError| TestCaseError::fail(format!("{e:?}")))
          .flat_map(move |got| {
            prop_sync(move || {
              prop_assert_eq!(got, expected_front);
              Ok(())
            })
          }),
        Cmd::Wait(ms) => sleep(Duration::from_millis(*ms)),
      };
      step.then(q2.size()).flat_map(move |len| {
        prop_sync(move || {
          prop_assert_eq!(len, expected_len);
          Ok(())
        })
      })
    }
  }

  #[test]
  fn bounded_queue_matches_its_model() {
    run_state_machine(&BoundedQueue { cap: 3 }, Config::with_cases(64), 0..40);
  }

  proptest! {
    #[test]
    fn check_commands_inside_proptest(cmds in commands(&BoundedQueue { cap: 2 }, 0..20)) {
      check_commands(&BoundedQueue { cap: 2 }, &cmds)?;
    }
  }

  /// A model that is wrong once the queue holds two items.
  struct WrongModel;

  impl StateMachine for WrongModel {
    type State = usize;
    type Sut = Queue<u8>;
    type Command = u8;

    fn initial_state(&self) -> usize {
      0
    }

    fn commands(&self) -> BoxedStrategy<u8> {
      any::<u8>().boxed()
    }

    fn next_state(&self, state: &usize, _cmd: &u8) -> usize {
      (state + 1).min(1)
    }

    fn init_sut(&self) -> Effect<Queue<u8>, TestCaseError> {
      succeed(Queue::unbounded())
    }

    fn apply(
      &self,
      q: &Queue<u8>,
      cmd: &u8,
      _: &usize,
      after: &usize,
    ) -> Effect<(), TestCaseError> {
      let expected = *after;
      q.offer(*cmd).then(q.size()).flat_map(move |len| {
        prop_sync(move || {
          prop_assert_eq!(len, expected);
          Ok(())
        })
      })
    }
  }

  #[test]
  fn a_failing_machine_shrinks_to_two_commands() {
    let config = Config {
      failure_persistence: None,
      ..Config::with_cases(64)
    };
    let run = std::panic::AssertUnwindSafe(|| run_state_machine(&WrongModel, config, 0..20));
    let panic = std::panic::catch_unwind(run).expect_err("the wrong model must fail");
    let msg = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(msg.contains("step 1"), "{msg}");
    assert!(
      msg.contains("minimal failing commands: [\n    0,\n    0,\n]"),
      "{msg}"
    );
  }

  #[test]
  fn check_commands_skips_commands_whose_precondition_fails() {
    let machine = BoundedQueue { cap: 1 };
    let cmds = [Cmd::Offer(1), Cmd::Offer(2), Cmd::Poll, Cmd::Poll];
    assert!(check_commands(&machine, &cmds).is_ok());
  }
}
