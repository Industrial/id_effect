//! Effect-ts-style **services**: a stable key type `K` and payload `V` as [`Service<K, V>`] (a [`Tagged`]
//! cell in the environment [`Context`]).
//!
//! This mirrors `Context.Tag("Name")<Self, Interface>()` + `Layer.succeed(tag, impl)` in Effect.ts:
//! - **`K`**: zero-sized key (your tag identity). Keys should be created with [`service_key!`](macro@crate::service_key);
//!   they implement [`crate::schema::equal::Equal`] and [`crate::schema::equal::EffectHash`] like [`crate::Brand`]-style nominal tags,
//!   so you can use `HashSet<Service<K, V>>` when `V` is [`Hash`](std::hash::Hash).
//! - **`V`**: the implementation — prefer **one aggregate** (fields + methods) per logical service rather than
//!   many micro-tags in a long [`Cons`] list. When treating a [`Service`] cell as structural data (for example
//!   [`HashSet`](std::collections::HashSet) deduplication), bound **`V`: [`crate::schema::data::EffectData`]** so equality
//!   and hashing match the `EffectData` protocol (equivalently `PartialEq + Eq + Hash` on `V`).
//! - **Provide** at the composition root with [`service_env`] (single cell) or stack cells with [`crate::layer::Layer`].
//! - Run [`crate::kernel::Effect`] with `R` = [`ServiceEnv<K, V>`] or [`crate::context::Context<…>`] and use
//!   [`Context::get`](crate::context::Context::get) / [`get_mut`](crate::context::Context::get_mut) on `K`.
//!
//! ## Example
//!
//! ```ignore
//! id_effect::service_key!(pub struct DbKey);
//! type Db = id_effect::Service<DbKey, i32>;
//!
//! let env = id_effect::Context::new(id_effect::Cons(
//!   id_effect::Tagged::<DbKey, _>::new(42),
//!   id_effect::Nil,
//! ));
//! ```

use core::convert::Infallible;

use crate::context::{Cons, Context, Nil};
use crate::kernel::Effect;
use crate::schema::equal::EffectHash;

pub use crate::context::Tagged;

/// One dependency slot: key `K` holding value `V` (Effect.ts tag + service implementation).
///
/// [`crate::schema::data::EffectData`] applies to `Service<K, V>` when `V` does (via the blanket impl and
/// [`Tagged`]'s derives), which is the right bound for keyed collections over full cells.
pub type Service<K, V> = Tagged<K, V>;

/// Build a single service cell (`Context.Tag` payload).
#[inline]
pub fn service<K: EffectHash, V>(value: V) -> Service<K, V> {
  Service::<K, V>::new(value)
}

/// Environment holding exactly one [`Service<K, V>`] (Effect.ts: one tag satisfied; **provide** at the root).
pub type ServiceEnv<K, V> = Context<Cons<Service<K, V>, Nil>>;

/// Wrap `v` as the only cell in a [`Context`] — **provide** this as `R` for effects that require [`ServiceEnv<K, V>`].
#[inline]
pub fn service_env<K: EffectHash, V>(v: V) -> ServiceEnv<K, V> {
  Context::new(Cons(service::<K, V>(v), Nil))
}

/// [`Layer`](crate::layer::Layer) that builds a full [`ServiceEnv<K, V>`] (cloneable payload).
#[inline]
pub fn layer_service_env<K: EffectHash, V: Clone>(
  v: V,
) -> crate::layer::LayerFn<impl Fn() -> Result<ServiceEnv<K, V>, Infallible>> {
  crate::layer::LayerFn(move || Ok(service_env::<K, V>(v.clone())))
}

/// [`Layer`](crate::layer::Layer) that provides a single infallible [`Service`] cell (Effect.ts `Layer.succeed`).
///
/// `V` must be [`Clone`] because [`Layer::build`](crate::layer::Layer::build) may run more than once.
#[inline]
pub fn layer_service<K: EffectHash, V: Clone>(
  value: V,
) -> crate::layer::LayerFn<impl Fn() -> Result<Service<K, V>, Infallible>> {
  crate::layer::LayerFn(move || Ok(service::<K, V>(value.clone())))
}

/// Ergonomic helper mirroring `Effect::provide_service` as a free function.
#[inline]
#[allow(clippy::type_complexity)]
pub fn provide_service<K: EffectHash, V, A, E, Tail>(
  effect: Effect<A, E, Context<Cons<Service<K, V>, Tail>>>,
  value: V,
) -> Effect<A, E, Context<Tail>>
where
  A: 'static,
  E: 'static,
  V: Clone + 'static,
  Tail: Clone + 'static,
{
  effect.provide_head(value)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::context::{Cons, Context, Nil};
  use crate::layer::Layer;
  use crate::schema::data::EffectData;
  use crate::schema::equal::{EffectHash, equals};
  use rstest::rstest;
  use std::collections::HashSet;

  crate::service_key!(struct PortKey);
  crate::service_def!(struct HttpKey as HttpService => u16);

  mod constructors {
    use super::*;

    #[rstest]
    #[case::default_port(8080u16)]
    #[case::alternate_port(3000u16)]
    fn layer_service_with_clonable_value_builds_tagged_cell(#[case] port: u16) {
      let layer = layer_service::<PortKey, _>(port);
      let cell = layer.build().expect("layer service should build");
      assert_eq!(cell.value, port);
    }

    #[test]
    fn service_def_macro_defines_key_and_service_alias() {
      let svc: HttpService = service::<HttpKey, _>(8080u16);
      assert_eq!(svc.value, 8080u16);
    }
  }

  mod environment_access {
    use super::*;

    #[test]
    fn context_get_with_service_cell_resolves_service_value() {
      let ctx = Context::new(Cons(Service::<PortKey, _>::new(9u8), Nil));
      assert_eq!(*ctx.get::<PortKey>(), 9);
    }

    #[test]
    fn service_env_with_value_matches_manual_context_layout() {
      let a = service_env::<PortKey, _>(7u8);
      let b = Context::new(Cons(Service::<PortKey, _>::new(7u8), Nil));
      assert_eq!(*a.get::<PortKey>(), *b.get::<PortKey>());
    }

    #[test]
    fn layer_service_env_with_value_builds_context_with_single_service_cell() {
      let layer = layer_service_env::<PortKey, _>(77u8);
      let env = layer.build().expect("service env layer should build");
      assert_eq!(*env.get::<PortKey>(), 77u8);
    }
  }

  mod providing {
    use super::*;

    #[test]
    fn provide_service_helper_with_effect_matches_method_semantics() {
      let effect = Effect::new(|ctx: &mut Context<Cons<Service<PortKey, u8>, Nil>>| {
        Ok::<u8, ()>(*ctx.get::<PortKey>())
      });

      let provided = provide_service(effect, 42u8);
      let out = crate::runtime::run_blocking(provided, Context::new(Nil));
      assert_eq!(out, Ok(42));
    }
  }

  /// Brand / `Equal` / `EffectHash` semantics for `service_key!` types and `HashSet<Service<…>>`.
  mod brand_equal_and_hashset {
    use super::*;

    fn assert_key_bounds<K: EffectHash + Eq + Copy>() {}

    #[test]
    fn service_key_struct_eq_by_value() {
      assert_key_bounds::<PortKey>();
      let a = PortKey;
      let b = PortKey;
      assert!(equals(&a, &b));
      assert_eq!(a, b);
      assert_eq!(EffectHash::effect_hash(&a), EffectHash::effect_hash(&b));
    }

    #[test]
    fn two_distinct_service_keys_not_equal() {
      let x = service::<PortKey, _>(1u8);
      let y = service::<PortKey, _>(2u8);
      assert_ne!(x, y);
      assert!(!equals(&x, &y));
    }

    #[test]
    fn hashset_of_service_keys_deduplicates() {
      let mut set = HashSet::new();
      set.insert(service::<PortKey, _>(9u8));
      set.insert(service::<PortKey, _>(9u8));
      assert_eq!(set.len(), 1);
      assert!(set.contains(&service::<PortKey, _>(9u8)));
    }

    #[test]
    fn service_cell_is_effect_data_when_payload_is_effect_data() {
      fn assert_effect_data<T: EffectData>() {}
      assert_effect_data::<Service<PortKey, u16>>();
    }
  }
}
