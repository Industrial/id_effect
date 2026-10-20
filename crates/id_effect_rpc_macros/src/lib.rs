//! Procedural macros for [`id_effect_rpc`](https://docs.rs/id_effect_rpc); use them through
//! `id_effect_rpc`, which re-exports both.
//!
//! - `#[service(version = N)]` (written `#[rpc::service(version = N)]`) turns a trait of
//!   procedures into a typed RPC service: the handler trait, `<Trait>Service` (contract,
//!   registration, router mount) and `<Trait>Client<T>` (ADR 0014).
//! - `#[derive(RpcErrorUnion)]` makes an enum a typed error union (design note §4).
//!
//! Every procedure carries one kind (`#[query]`, `#[mutation]`, `#[server_stream]`,
//! `#[client_stream]`, `#[bidi]`, `#[subscription]`) and one access decision (`#[public]` or one
//! or more `#[guard(G)]`); leaving the decision out is a compile error (ADR 0013 R1).
//!
//! ```rust
//! use id_effect::{DynEnv, Effect, Never, succeed};
//! use id_effect_rpc as rpc;
//!
//! #[rpc::service(version = 1)]
//! trait Greeter {
//!   #[query]
//!   #[public]
//!   fn greet(&self, name: String) -> Effect<String, Never, DynEnv>;
//! }
//!
//! struct Hello;
//!
//! impl Greeter for Hello {
//!   fn greet(&self, name: String) -> Effect<String, Never, DynEnv> {
//!     succeed(format!("Hello, {name}!"))
//!   }
//! }
//!
//! let group = GreeterService::group(Hello);
//! assert_eq!(group.tags().collect::<Vec<_>>(), ["Greeter.greet"]);
//! assert_eq!(GreeterService::contract().procedures[0].name, "greet");
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use proc_macro::TokenStream;

/// The README's example runs as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

mod error;
mod service;
mod support;

/// Declares an RPC service on a trait: `#[rpc::service(version = N)]`, optionally with
/// `name = "Service"` (default: the trait name).
///
/// Each method is a procedure with one kind attribute and one access decision, and returns an
/// `Effect<A, E, DynEnv>`; inputs, outputs and items are `String`, `bool`, `i64`, `f64`, `()`,
/// `Option`/`Vec` of those, or `#[derive(id_effect::Schema)]` types, and `E` is
/// `#[derive(RpcErrorUnion)]` (or `Never`). See `id_effect_rpc::service` for what it generates.
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
  service::expand(attr.into(), item.into())
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

/// Implements `RpcErrorStatus`, `RpcErrorUnion` and `RpcErrorDecode` for an enum of unit and
/// named-field variants: `{"_tag": "<Variant>", …fields}` on the wire.
///
/// `#[rpc(status = N)]` on the enum or a variant sets the HTTP status (default 400),
/// `#[rpc(tag = "…")]` renames a variant's tag, and `#[rpc(internal)]` marks a server fault
/// (answered as `InternalError { id }`, never sent). The reserved tags (`InternalError`,
/// `InvalidInput`, `DeadlineExceeded`, `Cancelled`, `ResourceExhausted`, `ResumeExpired`) are
/// refused.
#[proc_macro_derive(RpcErrorUnion, attributes(rpc))]
pub fn derive_rpc_error_union(item: TokenStream) -> TokenStream {
  error::expand(item.into())
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}
