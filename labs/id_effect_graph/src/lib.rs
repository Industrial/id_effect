//! Directed acyclic graphs and topological sorting.
//!
//! | Type / fn | Role |
//! |-----------|------|
//! | [`Dag`] | DAG with explicit dependency edges |
//! | [`DependencyNode`] + [`topological_sort`] | capability-style topological sort |
//! | [`GraphError`] | planning errors |

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod dag;
mod error;
mod topological_sort;

pub use dag::Dag;
pub use error::GraphError;
pub use topological_sort::{DependencyNode, topological_sort};
