//! `providers!` macro — named provider bundles.

/// Expand a named provider bundle to a `[ProviderBox]` array.
///
/// ```ignore
/// run_with(providers!(dev: [DbLive, LoggerLive]), app())
/// ```
#[macro_export]
macro_rules! providers {
  ($name:ident: [$($provider:ty),* $(,)?]) => {
    [$(::id_effect::ProviderBox::new::<$provider>(),)*]
  };
}
