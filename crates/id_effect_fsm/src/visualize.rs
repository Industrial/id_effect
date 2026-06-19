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

#[cfg(test)]
mod visualize_tests {
  use super::*;
  use crate::machine::{StateMachine, TransitionTable};

  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  enum S {
    Idle,
    On,
  }

  impl std::fmt::Display for S {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "{self:?}")
    }
  }

  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  enum E {
    Toggle,
  }

  impl std::fmt::Display for E {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "{self:?}")
    }
  }

  fn lamp_machine() -> StateMachine<S, E> {
    let table = TransitionTable::new()
      .on(S::Idle, E::Toggle, S::On)
      .on(S::On, E::Toggle, S::Idle);
    StateMachine::new(S::Idle, table)
  }

  #[test]
  fn to_mermaid_includes_initial_and_edges() {
    let m = lamp_machine();
    let diagram = to_mermaid(&m, |s| format!("{s:?}"), |e| format!("{e:?}"));
    assert!(diagram.contains("stateDiagram-v2"));
    assert!(diagram.contains("[*] --> Idle"));
    assert!(diagram.contains("Idle --> On: Toggle"));
  }

  #[test]
  fn mermaid_id_quotes_special_labels() {
    assert_eq!(mermaid_id("ok_state"), "ok_state");
    assert_eq!(mermaid_id("has space"), r#""has space""#);
    assert_eq!(mermaid_id(r#"say "hi""#), r#""say \"hi\"""#);
  }

  #[test]
  fn to_mermaid_display_uses_display() {
    let m = lamp_machine();
    let diagram = to_mermaid_display(&m);
    assert!(diagram.contains("Idle"));
  }
}
