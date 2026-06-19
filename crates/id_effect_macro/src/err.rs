//! `err!` macro.

/// Type-level error sum ("union") using nested [`Or`](::id_effect::Or).
///
/// ```ignore
/// type E = id_effect::err!(IoErr | DecodeErr);
/// ```
#[macro_export]
macro_rules! err {
  () => { () };
  ($e:ty) => { $e };
  ($e:ty | $($rest:tt)+) => {
    ::id_effect::Or<$e, $crate::err!($($rest)+)>
  };
}
