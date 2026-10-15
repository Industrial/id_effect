//! Managed resources on top of [`Scope`](crate::next::Scope): `Cache`, `Pool` and `KeyedPool`.
//! Empty until `c-resource` fills it.
//!
//! Constructors are plain functions; a pooled item is released by a finalizer, so it returns to
//! the pool on interrupt too. `Scope` itself belongs to the kernel (`next::scope`).
