//! `define_capability!` macro.

/// Declare a capability key and optional stored value type.
#[macro_export]
macro_rules! define_capability {
  ($(#[$m:meta])* $key:ident, $value:ty) => {
    $(#[$m])*
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct $key;
    impl ::id_effect::CapabilityKey for $key {
      type Value = $value;
    }
  };
  ($(#[$m:meta])* $key:ident) => {
    $(#[$m])*
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct $key;
  };
}
