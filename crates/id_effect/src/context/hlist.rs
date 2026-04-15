//! [`Nil`] / [`Cons`] — heterogeneous list spine (`Stratum 3.3`).

/// End of the heterogeneous list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Nil;

/// Head `H` followed by tail list `T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cons<H, T>(pub H, pub T);
