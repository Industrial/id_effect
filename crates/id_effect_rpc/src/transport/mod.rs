//! The transports of an [`RpcGroup`](crate::RpcGroup) besides the HTTP routes of
//! [`RpcServer`](crate::RpcServer). Every transport runs its calls through
//! [`dispatch`](crate::server::dispatch), so guards, decoding, typed errors, deadlines and
//! cancellation are those of the HTTP routes.
//!
//! | Module | Feature | What |
//! |---|---|---|
//! | `mesh` | `mesh` | the same contract node to node over the process mesh (postcard always) |

// r12-e12-2-mesh-rpc: RPC endpoints as mesh processes under a cluster-wide name.
#[cfg(feature = "mesh")]
pub mod mesh;
