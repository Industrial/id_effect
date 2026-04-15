//! `req!` macro.

/// Type-level required service stack (Rust equivalent of `R` union requirements).
///
/// ```ignore
/// type R = id_effect::req!(DbKey: DbClient | LogKey: Logger);
/// ```
#[macro_export]
macro_rules! req {
  (@tail $k:ty : $v:ty) => {
    ::id_effect::Cons<::id_effect::Service<$k, $v>, ::id_effect::Nil>
  };
  (@tail $k:ty : $v:ty | $($rest:tt)+) => {
    ::id_effect::Cons<::id_effect::Service<$k, $v>, $crate::req!(@tail $($rest)+)>
  };
  () => {
    ::id_effect::Context<::id_effect::Nil>
  };
  ($($pairs:tt)+) => {
    ::id_effect::Context<$crate::req!(@tail $($pairs)+)>
  };
}
