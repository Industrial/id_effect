//! Type-level paths into an HList (`Stratum 3.4`).

use core::marker::PhantomData;

/// Path pointing at this [`Cons`](super::hlist::Cons) cell’s head.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Here;

/// Path: skip this head, then follow inner path `P`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct There<P>(pub PhantomData<P>);

/// `Skip0 = Here` (SPEC) — head cell.
pub type Skip0 = Here;

/// Type alias for `There<Here>` (second cell in the list).
pub type ThereHere = There<Here>;
/// Skip one tail link (`ThereHere`).
pub type Skip1 = ThereHere;
/// Skip two links (third cell is [`Here`] on the inner tail).
pub type Skip2 = There<ThereHere>;
/// Skip three links (fourth cell).
pub type Skip3 = There<Skip2>;
/// Skip four links (fifth cell).
pub type Skip4 = There<Skip3>;
