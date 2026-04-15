//! `service_def!` macro.

/// Define a service key and ergonomic service type alias in one declaration.
///
/// ```ignore
/// id_effect::service_def!(
///   pub struct ClockKey as ClockService => std::time::Duration
/// );
/// // Generates:
/// // - `pub struct ClockKey;`
/// // - `pub type ClockService = id_effect::Service<ClockKey, std::time::Duration>;`
/// ```
#[macro_export]
macro_rules! service_def {
  ($(#[$m:meta])* $vis:vis struct $name:ident as $alias:ident => $ty:ty) => {
    $crate::service_key!($(#[$m])* $vis struct $name);
    $vis type $alias = ::id_effect::Service<$name, $ty>;
  };
}
