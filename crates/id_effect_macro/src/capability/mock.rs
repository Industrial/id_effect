//! `mock_capability!` macro.

/// Generate a [`ProviderSpec`](::id_effect::ProviderSpec) with a closure body.
///
/// ```ignore
/// mock_capability!(CounterMock, Counter, "counter/mock", || Counter(7));
/// let env = id_effect::build_env([id_effect::provide!(CounterMock)]).unwrap();
/// ```
#[macro_export]
macro_rules! mock_capability {
  ($provider:ident, $service:ty, $id:expr, $provide:expr) => {
    struct $provider;

    impl ::id_effect::ProviderSpec for $provider {
      type Key = ::id_effect::Cap<$service>;
      type Output = $service;

      fn provider_id() -> &'static str {
        $id
      }

      fn provide(
        _deps: &::id_effect::Env,
      ) -> ::core::result::Result<$service, ::id_effect::ProviderError> {
        Ok(($provide)())
      }
    }
  };
}
