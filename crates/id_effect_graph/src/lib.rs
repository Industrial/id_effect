//! Directed acyclic graphs and topological sorting.
//!
//! | Module | Role |
//! |--------|------|
//! | [`dag`] | [`Dag`] with explicit dependency edges |
//! | [`topological_sort`] | [`DependencyNode`] + capability-style [`topological_sort`] |
//! | [`error`] | [`GraphError`] |

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod dag;
mod error;
mod topological_sort;

pub use dag::Dag;
pub use error::GraphError;
pub use topological_sort::{DependencyNode, topological_sort};
