//! `provide!` macro.

/// Wrap a [`ProviderSpec`](::id_effect::ProviderSpec) as a [`ProviderBox`](::id_effect::ProviderBox).
#[macro_export]
macro_rules! provide {
  ($provider:ty) => {
    ::id_effect::ProviderBox::new::<$provider>()
  };
}
