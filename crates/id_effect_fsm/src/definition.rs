//! The [`Fsm`] trait: a state type that knows its initial state and transition table.
//!
//! Usually implemented with `#[derive(Fsm)]` (re-exported from this crate):
//!
//! ```rust
//! use id_effect_fsm::Fsm;
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
//! enum DoorEvent { Open, Close, Lock, Unlock }
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
//! #[fsm(event = DoorEvent, initial = Closed)]
//! enum Door {
//!   #[fsm(on(Open => Opened, Lock => Locked))]
//!   Closed,
//!   #[fsm(on(Close => Closed))]
//!   Opened,
//!   #[fsm(on(Unlock => Closed))]
//!   Locked,
//! }
//!
//! let mut door = Door::state_machine();
//! assert_eq!(door.step(DoorEvent::Lock), Ok(Door::Locked));
//! assert!(door.step(DoorEvent::Open).is_err());
//! assert_eq!(Door::transition_table().len(), 4);
//! ```
//!
//! Derive syntax:
//!
//! - container `#[fsm(event = EventType, initial = Variant, crate = path)]` — `event` is
//!   required, `initial` defaults to the first variant, `crate` overrides the
//!   `::id_effect_fsm` path used by the generated code (for re-exporting crates);
//! - variant `#[fsm(on(Event => Target, ..))]`, repeatable — a bare `Event` means
//!   `EventType::Event`, a multi-segment path is used as written.
//!
//! Unknown targets, an unknown `initial`, data-carrying variants, generic enums and duplicate
//! `(state, event)` edges are rejected at compile time.

use crate::machine::{StateMachine, TransitionTable};
use std::fmt::Debug;
use std::hash::Hash;

/// A finite set of states with a fixed initial state and transition table.
pub trait Fsm: Copy + Eq + Hash + Debug + Sized + 'static {
  /// Events that drive transitions.
  type Event: Copy + Eq + Hash + Debug + 'static;

  /// The state a fresh machine starts in.
  fn initial() -> Self;

  /// Every `(state, event) -> state` edge.
  fn transition_table() -> TransitionTable<Self, Self::Event>;

  /// A fresh [`StateMachine`] at [`Fsm::initial`] over [`Fsm::transition_table`].
  fn state_machine() -> StateMachine<Self, Self::Event> {
    StateMachine::new(Self::initial(), Self::transition_table())
  }
}
