//! Procedural macros for [`id_effect_rpc`](https://docs.rs/id_effect_rpc).
//!
//! `#[service]` (used as `#[rpc::service(version = N)]` through `id_effect_rpc`) turns a trait of
//! procedures into a typed RPC service: the handler trait, the Rust client, the contract and the
//! router mount (ADR 0014).
//!
//! Status (0.12 development): the attribute accepts a trait and emits it unchanged. Procedure
//! kinds (`#[query]`, `#[mutation]`, `#[server_stream]`, `#[client_stream]`, `#[bidi]`,
//! `#[subscription]`) and access decisions (`#[public]`, `#[guard(G)]`) arrive with leaf
//! `r12-e12-1-macro`.
//!
//! ```rust
//! #[id_effect_rpc_macros::service]
//! trait Greeter {
//!   fn greet(&self, name: String) -> String;
//! }
//!
//! struct Hello;
//! impl Greeter for Hello {
//!   fn greet(&self, name: String) -> String {
//!     format!("hello, {name}")
//!   }
//! }
//!
//! assert_eq!(Hello.greet("rpc".into()), "hello, rpc");
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use proc_macro::TokenStream;

mod service;

/// Declares an RPC service on a trait.
///
/// Only traits are accepted; anything else is a compile error that points at the item.
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
  service::expand(attr.into(), item.into())
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}
