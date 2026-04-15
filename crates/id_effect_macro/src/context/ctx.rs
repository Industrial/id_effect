//! `ctx!` macro.

/// Runtime context builder with service cells.
///
/// ```ignore
/// let r = id_effect::ctx!(
///   LogKey => Logger::default(),
///   DbKey => db,
/// );
/// ```
#[macro_export]
macro_rules! ctx {
  () => {
    ::id_effect::Context::new(::id_effect::Nil)
  };
  ($k:ty => $v:expr $(, $rk:ty => $rv:expr )* $(,)?) => {
    ::id_effect::Context::new($crate::ctx!(@list $k => $v $(, $rk => $rv )*))
  };
  (@list $k:ty => $v:expr) => {
    ::id_effect::Cons(::id_effect::service::<$k, _>($v), ::id_effect::Nil)
  };
  (@list $k:ty => $v:expr, $($rest:tt)+) => {
    ::id_effect::Cons(::id_effect::service::<$k, _>($v), $crate::ctx!(@list $($rest)+))
  };
}
