//! Deprecated: software transactional memory moved to [`id_effect::stm`] in 0.11 (D5).
//!
//! The items here are aliases of the core ones (same types, same semantics) and are removed in
//! 0.12. Replace `id_effect_fp::stm::X` / `id_effect_fp::X` with `id_effect::stm::X`.

use id_effect::Effect;

/// Deprecated alias of [`id_effect::stm::Txn`].
#[deprecated(since = "0.11.0", note = "use id_effect::stm")]
pub type Txn = id_effect::stm::Txn;

/// Deprecated alias of [`id_effect::stm::Outcome`].
#[deprecated(since = "0.11.0", note = "use id_effect::stm")]
pub type Outcome<A, E> = id_effect::stm::Outcome<A, E>;

/// Deprecated alias of [`id_effect::stm::Stm`].
#[deprecated(since = "0.11.0", note = "use id_effect::stm")]
pub type Stm<A, E> = id_effect::stm::Stm<A, E>;

/// Deprecated alias of [`id_effect::stm::TRef`].
#[deprecated(since = "0.11.0", note = "use id_effect::stm")]
pub type TRef<A> = id_effect::stm::TRef<A>;

/// Deprecated alias of [`id_effect::stm::TQueue`].
#[deprecated(since = "0.11.0", note = "use id_effect::stm")]
pub type TQueue<A> = id_effect::stm::TQueue<A>;

/// Deprecated alias of [`id_effect::stm::TMap`].
#[deprecated(since = "0.11.0", note = "use id_effect::stm")]
pub type TMap<K, V> = id_effect::stm::TMap<K, V>;

/// Deprecated alias of [`id_effect::stm::TSemaphore`].
#[deprecated(since = "0.11.0", note = "use id_effect::stm")]
pub type TSemaphore = id_effect::stm::TSemaphore;

/// Deprecated: calls [`id_effect::stm::commit`].
#[deprecated(since = "0.11.0", note = "use id_effect::stm")]
pub fn commit<A, E, R>(stm: id_effect::stm::Stm<A, E>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  id_effect::stm::commit(stm)
}

/// Deprecated: calls [`id_effect::stm::atomically`].
#[deprecated(since = "0.11.0", note = "use id_effect::stm")]
pub fn atomically<A, E, R>(stm: id_effect::stm::Stm<A, E>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  id_effect::stm::atomically(stm)
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
  use crate::test_support::run;

  #[test]
  fn deprecated_aliases_are_the_core_stm_items() {
    let cell: crate::TRef<i32> = run(crate::commit(crate::TRef::make(1))).expect("make");
    let core: id_effect::stm::TRef<i32> = cell.clone();
    run(super::atomically(core.update_stm::<(), _>(|n| n + 1))).expect("update");
    assert_eq!(run(id_effect::stm::commit(cell.read_stm::<()>())), Ok(2));
  }
}
