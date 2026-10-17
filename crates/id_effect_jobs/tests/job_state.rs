//! `JobState` column values and terminal states.

use id_effect_jobs::{JobState, UnknownJobState};

#[test]
fn every_state_round_trips_through_its_column_value() {
  for state in JobState::ALL {
    assert_eq!(state.as_str().parse::<JobState>(), Ok(state));
  }
  assert_eq!(
    "queued".parse::<JobState>(),
    Err(UnknownJobState("queued".to_owned()))
  );
}

#[test]
fn only_succeeded_and_dead_are_terminal() {
  let terminal: Vec<_> = JobState::ALL
    .into_iter()
    .filter(|s| s.is_terminal())
    .collect();
  assert_eq!(terminal, [JobState::Succeeded, JobState::Dead]);
}
