//! Prints every job state and whether it is terminal.
//!
//! `cargo run -p id_effect_jobs --example 010_job_states`

use id_effect_jobs::JobState;

fn main() {
  for state in JobState::ALL {
    let kind = if state.is_terminal() {
      "terminal"
    } else {
      "active"
    };
    println!("{state:<10} {kind}");
  }
}
