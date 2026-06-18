//! Mermaid diagram export for transition tables.

use crate::machine::{StateMachine, TransitionTable};
use std::fmt::Display;
use std::hash::Hash;

/// Renders a `stateDiagram-v2` Mermaid chart for `machine`.
pub fn to_mermaid<S, E, FS, FE>(
  machine: &StateMachine<S, E>,
  state_label: FS,
  event_label: FE,
) -> String
where
  S: Copy + Eq + Hash,
  E: Copy + Eq + Hash,
  FS: Fn(S) -> String,
  FE: Fn(E) -> String,
{
  table_to_mermaid(machine.table(), machine.initial(), state_label, event_label)
}

/// Renders a transition table (highlights `initial` with `[*]` entry).
pub fn table_to_mermaid<S, E, FS, FE>(
  table: &TransitionTable<S, E>,
  initial: S,
  state_label: FS,
  event_label: FE,
) -> String
where
  S: Copy + Eq + Hash,
  E: Copy + Eq + Hash,
  FS: Fn(S) -> String,
  FE: Fn(E) -> String,
{
  let mut out = String::from("stateDiagram-v2\n");
  out.push_str(&format!(
    "  [*] --> {}\n",
    mermaid_id(&state_label(initial))
  ));
  for (from, event, to) in table.edges() {
    out.push_str(&format!(
      "  {} --> {}: {}\n",
      mermaid_id(&state_label(from)),
      mermaid_id(&state_label(to)),
      event_label(event),
    ));
  }
  out
}

fn mermaid_id(label: &str) -> String {
  if label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
    label.to_string()
  } else {
    format!("\"{}\"", label.replace('"', "\\\""))
  }
}

/// Convenience when `S` and `E` implement [`Display`].
pub fn to_mermaid_display<S, E>(machine: &StateMachine<S, E>) -> String
where
  S: Copy + Eq + Hash + Display,
  E: Copy + Eq + Hash + Display,
{
  to_mermaid(machine, |s| s.to_string(), |e| e.to_string())
}
