//! Type-level capability set markers and [`CapList`] wrappers.

use super::env::Env;
use super::error::CapabilityError;
use super::key::CapabilityKey;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

/// Marker: effect requires capabilities listed in the `caps!` macro.
pub trait CapabilitySet {
  /// Verify `env` contains all required capabilities.
  fn verify(env: &Env) -> Result<(), CapabilityError>;
}

/// Build runtime `R` from a constructed [`Env`].
pub trait FromEnv: CapabilitySet + Sized {
  /// Wrap or pass through `env` for effect execution.
  fn from_env(env: Env) -> Self;
}

/// Tuple of capability keys — used as `CapList` type parameter.
pub trait CapKeys {
  /// Verify all keys in this tuple are present in `env`.
  fn verify_all(env: &Env) -> Result<(), CapabilityError>;
}

/// Empty capability set (pure effects use `()`).
pub struct NoCaps;

impl CapabilitySet for NoCaps {
  fn verify(_env: &Env) -> Result<(), CapabilityError> {
    Ok(())
  }
}

impl FromEnv for NoCaps {
  fn from_env(_env: Env) -> Self {
    Self
  }
}

impl CapabilitySet for () {
  fn verify(_env: &Env) -> Result<(), CapabilityError> {
    Ok(())
  }
}

impl FromEnv for () {
  fn from_env(_env: Env) -> Self {}
}

impl CapabilitySet for Env {
  fn verify(_env: &Env) -> Result<(), CapabilityError> {
    Ok(())
  }
}

impl FromEnv for Env {
  fn from_env(env: Env) -> Self {
    env
  }
}

impl CapKeys for () {
  fn verify_all(_env: &Env) -> Result<(), CapabilityError> {
    Ok(())
  }
}

macro_rules! impl_cap_keys {
  ($($k:ident),*) => {
    impl<$($k: CapabilityKey),*> CapKeys for ($($k,)*) {
      fn verify_all(env: &Env) -> Result<(), CapabilityError> {
        $( env.try_get::<$k>()?; )*
        Ok(())
      }
    }
  };
}

impl_cap_keys!(K0);
impl_cap_keys!(K0, K1);
impl_cap_keys!(K0, K1, K2);
impl_cap_keys!(K0, K1, K2, K3);
impl_cap_keys!(K0, K1, K2, K3, K4);
impl_cap_keys!(K0, K1, K2, K3, K4, K5);
impl_cap_keys!(K0, K1, K2, K3, K4, K5, K6);
impl_cap_keys!(K0, K1, K2, K3, K4, K5, K6, K7);
impl_cap_keys!(K0, K1, K2, K3, K4, K5, K6, K7, K8);
impl_cap_keys!(K0, K1, K2, K3, K4, K5, K6, K7, K8, K9);
impl_cap_keys!(K0, K1, K2, K3, K4, K5, K6, K7, K8, K9, K10);
impl_cap_keys!(K0, K1, K2, K3, K4, K5, K6, K7, K8, K9, K10, K11);
impl_cap_keys!(K0, K1, K2, K3, K4, K5, K6, K7, K8, K9, K10, K11, K12);
impl_cap_keys!(K0, K1, K2, K3, K4, K5, K6, K7, K8, K9, K10, K11, K12, K13);
impl_cap_keys!(
  K0, K1, K2, K3, K4, K5, K6, K7, K8, K9, K10, K11, K12, K13, K14
);
impl_cap_keys!(
  K0, K1, K2, K3, K4, K5, K6, K7, K8, K9, K10, K11, K12, K13, K14, K15
);

/// Environment typed with a tuple of required capability keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapList<Ks> {
  inner: Env,
  _marker: PhantomData<Ks>,
}

impl<Ks: CapKeys> CapList<Ks> {
  /// Wrap an already-built environment (after verification).
  #[inline]
  pub fn new(inner: Env) -> Self {
    Self {
      inner,
      _marker: PhantomData,
    }
  }

  /// Borrow inner [`Env`].
  #[inline]
  pub fn env(&self) -> &Env {
    &self.inner
  }

  /// Mutably borrow inner [`Env`].
  #[inline]
  pub fn env_mut(&mut self) -> &mut Env {
    &mut self.inner
  }
}

impl<Ks: CapKeys> Deref for CapList<Ks> {
  type Target = Env;

  fn deref(&self) -> &Env {
    &self.inner
  }
}

impl<Ks: CapKeys> DerefMut for CapList<Ks> {
  fn deref_mut(&mut self) -> &mut Env {
    &mut self.inner
  }
}

impl<Ks: CapKeys> CapabilitySet for CapList<Ks> {
  fn verify(env: &Env) -> Result<(), CapabilityError> {
    Ks::verify_all(env)
  }
}

impl<Ks: CapKeys> FromEnv for CapList<Ks> {
  fn from_env(env: Env) -> Self {
    Self::new(env)
  }
}

/// Helper trait: environment `R` exposes capability `K`.
pub trait HasCap<K: CapabilityKey> {}

impl<K: CapabilityKey> HasCap<K> for Env {}

impl<K: CapabilityKey, T: Deref<Target = Env>> HasCap<K> for T {}

/// Widen a capability set to a subset (subtyping).
pub trait CapWiden<Target> {
  /// Widen to a capability subset (structural subtyping).
  fn widen(self) -> Target;
}

impl<Ks: CapKeys> CapWiden<CapList<Ks>> for CapList<Ks> {
  fn widen(self) -> CapList<Ks> {
    self
  }
}

impl<K0: CapabilityKey, K1: CapabilityKey> CapWiden<CapList<(K0,)>> for CapList<(K0, K1)> {
  fn widen(self) -> CapList<(K0,)> {
    CapList::new(self.inner)
  }
}

/// Per-index single-key projection (see ADR 0005).
impl<K0: CapabilityKey> CapList<(K0,)> {
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_0(self) -> CapList<(K0,)> {
    CapList::new(self.inner)
  }
}

impl<K0: CapabilityKey, K1: CapabilityKey> CapList<(K0, K1)> {
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_0(self) -> CapList<(K0,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_1(self) -> CapList<(K1,)> {
    CapList::new(self.inner)
  }
}

impl<K0: CapabilityKey, K1: CapabilityKey, K2: CapabilityKey> CapList<(K0, K1, K2)> {
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_0(self) -> CapList<(K0,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_1(self) -> CapList<(K1,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_2(self) -> CapList<(K2,)> {
    CapList::new(self.inner)
  }
}

impl<K0: CapabilityKey, K1: CapabilityKey, K2: CapabilityKey, K3: CapabilityKey>
  CapList<(K0, K1, K2, K3)>
{
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_0(self) -> CapList<(K0,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_1(self) -> CapList<(K1,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_2(self) -> CapList<(K2,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_3(self) -> CapList<(K3,)> {
    CapList::new(self.inner)
  }
}

impl<K0: CapabilityKey, K1: CapabilityKey, K2: CapabilityKey, K3: CapabilityKey, K4: CapabilityKey>
  CapList<(K0, K1, K2, K3, K4)>
{
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_0(self) -> CapList<(K0,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_1(self) -> CapList<(K1,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_2(self) -> CapList<(K2,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_3(self) -> CapList<(K3,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_4(self) -> CapList<(K4,)> {
    CapList::new(self.inner)
  }
}

impl<
  K0: CapabilityKey,
  K1: CapabilityKey,
  K2: CapabilityKey,
  K3: CapabilityKey,
  K4: CapabilityKey,
  K5: CapabilityKey,
> CapList<(K0, K1, K2, K3, K4, K5)>
{
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_0(self) -> CapList<(K0,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_1(self) -> CapList<(K1,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_2(self) -> CapList<(K2,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_3(self) -> CapList<(K3,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_4(self) -> CapList<(K4,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_5(self) -> CapList<(K5,)> {
    CapList::new(self.inner)
  }
}

impl<
  K0: CapabilityKey,
  K1: CapabilityKey,
  K2: CapabilityKey,
  K3: CapabilityKey,
  K4: CapabilityKey,
  K5: CapabilityKey,
  K6: CapabilityKey,
> CapList<(K0, K1, K2, K3, K4, K5, K6)>
{
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_0(self) -> CapList<(K0,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_1(self) -> CapList<(K1,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_2(self) -> CapList<(K2,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_3(self) -> CapList<(K3,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_4(self) -> CapList<(K4,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_5(self) -> CapList<(K5,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_6(self) -> CapList<(K6,)> {
    CapList::new(self.inner)
  }
}

impl<
  K0: CapabilityKey,
  K1: CapabilityKey,
  K2: CapabilityKey,
  K3: CapabilityKey,
  K4: CapabilityKey,
  K5: CapabilityKey,
  K6: CapabilityKey,
  K7: CapabilityKey,
> CapList<(K0, K1, K2, K3, K4, K5, K6, K7)>
{
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_0(self) -> CapList<(K0,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_1(self) -> CapList<(K1,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_2(self) -> CapList<(K2,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_3(self) -> CapList<(K3,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_4(self) -> CapList<(K4,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_5(self) -> CapList<(K5,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_6(self) -> CapList<(K6,)> {
    CapList::new(self.inner)
  }
  #[inline]
  /// Project to the capability at this index (shared [`Env`], type-level single-key subset).
  pub fn project_at_7(self) -> CapList<(K7,)> {
    CapList::new(self.inner)
  }
}

impl<K0: CapabilityKey, K1: CapabilityKey, K2: CapabilityKey> CapWiden<CapList<(K0,)>>
  for CapList<(K0, K1, K2)>
{
  fn widen(self) -> CapList<(K0,)> {
    CapList::new(self.inner)
  }
}

impl<K0: CapabilityKey, K1: CapabilityKey, K2: CapabilityKey> CapWiden<CapList<(K0, K1)>>
  for CapList<(K0, K1, K2)>
{
  fn widen(self) -> CapList<(K0, K1)> {
    CapList::new(self.inner)
  }
}

#[cfg(test)]
#[allow(dead_code, clippy::new_ret_no_self)]
mod tests {
  use super::*;
  #[::id_effect::capability(u32)]
  struct TestKey;

  #[test]
  fn cap_list_verify_missing() {
    let env = Env::new();
    let err = CapList::<(TestKeyKey,)>::verify(&env).unwrap_err();
    assert!(matches!(err, CapabilityError::Missing(_)));
  }

  #[test]
  fn cap_list_verify_ok() {
    let mut env = Env::new();
    env.insert::<TestKeyKey>(7u32);
    CapList::<(TestKeyKey,)>::verify(&env).unwrap();
  }

  #[test]
  fn cap_list_eight_keys() {
    #[::id_effect::capability(u8)]
    struct Cap0;
    #[::id_effect::capability(u8)]
    struct Cap1;
    #[::id_effect::capability(u8)]
    struct Cap2;
    #[::id_effect::capability(u8)]
    struct Cap3;
    #[::id_effect::capability(u8)]
    struct Cap4;
    #[::id_effect::capability(u8)]
    struct Cap5;
    #[::id_effect::capability(u8)]
    struct Cap6;
    #[::id_effect::capability(u8)]
    struct Cap7;
    let mut env = Env::new();
    env.insert::<Cap0Key>(0u8);
    env.insert::<Cap1Key>(1u8);
    env.insert::<Cap2Key>(2u8);
    env.insert::<Cap3Key>(3u8);
    env.insert::<Cap4Key>(4u8);
    env.insert::<Cap5Key>(5u8);
    env.insert::<Cap6Key>(6u8);
    env.insert::<Cap7Key>(7u8);
    CapList::<(
      Cap0Key,
      Cap1Key,
      Cap2Key,
      Cap3Key,
      Cap4Key,
      Cap5Key,
      Cap6Key,
      Cap7Key,
    )>::verify(&env)
    .unwrap();
  }

  #[test]
  fn no_caps_and_unit_from_env() {
    assert!(NoCaps::verify(&Env::new()).is_ok());
    assert!(<() as CapabilitySet>::verify(&Env::new()).is_ok());
    let _ = NoCaps::from_env(Env::new());
    let _ = <() as FromEnv>::from_env(Env::new());
  }

  #[test]
  fn env_as_capability_set() {
    let env = Env::new();
    assert!(Env::verify(&env).is_ok());
    assert_eq!(Env::from_env(env.clone()).len(), env.len());
  }

  #[test]
  fn cap_list_accessors_and_deref() {
    let mut env = Env::new();
    env.insert::<TestKeyKey>(3u32);
    let mut caps = CapList::<(TestKeyKey,)>::from_env(env);
    assert_eq!(caps.env().len(), 1);
    caps.env_mut().insert::<TestKeyKey>(4u32);
    assert_eq!(*caps.get::<TestKeyKey>(), 4);
  }

  #[test]
  fn cap_list_project_four_through_eight() {
    #[::id_effect::capability(u8)]
    struct A;
    #[::id_effect::capability(u8)]
    struct B;
    #[::id_effect::capability(u8)]
    struct C;
    #[::id_effect::capability(u8)]
    struct D;
    #[::id_effect::capability(u8)]
    struct E;
    #[::id_effect::capability(u8)]
    struct F;
    #[::id_effect::capability(u8)]
    struct G;
    #[::id_effect::capability(u8)]
    struct H;
    let mut env = Env::new();
    env.insert::<AKey>(1);
    env.insert::<BKey>(2);
    env.insert::<CKey>(3);
    env.insert::<DKey>(4);
    env.insert::<EKey>(5);
    env.insert::<FKey>(6);
    env.insert::<GKey>(7);
    env.insert::<HKey>(8);
    CapList::<(AKey, BKey, CKey, DKey, EKey)>::verify(&env).unwrap();
    let wide8 = CapList::<(AKey, BKey, CKey, DKey, EKey, FKey, GKey, HKey)>::from_env(env.clone());
    let _ = wide8.clone().project_at_4();
    let _ = wide8.clone().project_at_5();
    let _ = wide8.clone().project_at_6();
    let _ = wide8.clone().project_at_7();
    let four = CapList::<(AKey, BKey, CKey, DKey)>::from_env(env);
    let _ = four.clone().project_at_0();
    let _ = four.clone().project_at_1();
    let _ = four.clone().project_at_2();
    let _ = four.project_at_3();
  }
  #[test]
  fn cap_widen_subset() {
    #[::id_effect::capability(u32)]
    struct Db;
    #[::id_effect::capability(u32)]
    struct Log;
    let mut env = Env::new();
    env.insert::<DbKey>(1u32);
    env.insert::<LogKey>(2u32);
    let wide = CapList::<(DbKey, LogKey)>::from_env(env);
    let _narrow: CapList<(DbKey,)> = wide.widen();
  }
}
