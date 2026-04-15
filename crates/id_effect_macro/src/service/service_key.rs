//! `service_key!` macro.

/// Declare a zero-sized service key type (Effect.ts tag identity).
///
/// Generated structs use **`PartialEq` / `Eq` / `Hash`** derives so they participate in
/// [`Equal`](::id_effect::Equal) and [`EffectHash`](::id_effect::EffectHash) via the blanket impls—the
/// same structural equality story as [`Brand`](::id_effect::Brand) for nominal, type-level tags (all
/// values of a given key type are equal; distinct key types are distinct at compile time).
///
/// ```ignore
/// id_effect::service_key!(/// Pool handle
/// pub struct PgPoolKey);
/// type PgPoolSvc = id_effect::Service<PgPoolKey, sqlx::PgPool>;
/// ```
#[macro_export]
macro_rules! service_key {
  ($(#[$m:meta])* $vis:vis struct $name:ident) => {
    $(#[$m])*
    #[doc = "Nominal service key (`service_key!`). Implements [`Equal`](::id_effect::Equal) and [`EffectHash`](::id_effect::EffectHash) through the standard derives—Brand-style structural equality for this ZST tag."]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    $vis struct $name;
  };
}
