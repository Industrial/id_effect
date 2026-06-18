//! `mock_capability!` macro.

/// Generate a [`ProviderSpec`](::id_effect::ProviderSpec) with a closure body.
///
/// ```ignore
/// id_effect::define_capability!(CounterKey, u32);
/// mock_capability!(CounterMock, CounterKey, u32, "counter/mock", || 7);
/// let env = id_effect::build_env([id_effect::provide!(CounterMock)]).unwrap();
/// ```
#[macro_export]
macro_rules! mock_capability {
  ($provider:ident, $key:ty, $output:ty, $id:expr, $provide:expr) => {
    struct $provider;

    impl ::id_effect::ProviderSpec for $provider {
      type Key = $key;
      type Output = $output;

      fn provider_id() -> &'static str {
        $id
      }

      fn provide(_deps: &::id_effect::Env) -> Result<$output, ::id_effect::ProviderError> {
        Ok(($provide)())
      }
    }
  };
}
