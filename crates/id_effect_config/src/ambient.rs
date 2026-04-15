//! Thread-local ambient [`ConfigProvider`] for request- or test-scoped configuration.

use std::cell::RefCell;
use std::sync::Arc;

use id_effect::{Effect, box_future, effect};

use crate::provider::ConfigProvider;

type DynProvider = Arc<dyn ConfigProvider + Send + Sync + 'static>;

thread_local! {
  static AMBIENT_STACK: RefCell<Vec<DynProvider>> = const { RefCell::new(Vec::new()) };
}

struct AmbientPopGuard;

impl Drop for AmbientPopGuard {
  fn drop(&mut self) {
    AMBIENT_STACK.with(|s| {
      let _ = s.borrow_mut().pop();
    });
  }
}

/// Innermost provider installed by [`with_config_provider`], if any.
#[inline]
pub fn current_config_provider() -> Option<DynProvider> {
  AMBIENT_STACK.with(|s| s.borrow().last().cloned())
}

/// Run `inner` with `provider` as the ambient configuration source (see [`current_config_provider`]
/// and [`Config::load_current`](crate::Config::load_current)).
///
/// The ambient stack is installed around [`Effect::run`] on `inner` via [`Effect::new_async`]
/// inside one `effect!` block so the nested effect shares the same `&mut R` across `.await`
/// (not expressible with `from_async`, which requires a `'static` future).
pub fn with_config_provider<A, E, R>(
  inner: Effect<A, E, R>,
  provider: DynProvider,
) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  effect!(|r: &mut R| {
    let inner = inner;
    let provider = provider.clone();
    let out = ~Effect::new_async(move |env| {
      box_future(async move {
        AMBIENT_STACK.with(|s| s.borrow_mut().push(provider.clone()));
        let _guard = AmbientPopGuard;
        inner.run(env).await
      })
    });
    out
  })
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use id_effect::{Effect, run_blocking};

  use crate::ambient::{current_config_provider, with_config_provider};
  use crate::{Config, MapConfigProvider};

  #[test]
  fn with_config_provider_exposes_current_for_load_current() {
    let eff = with_config_provider(
      Effect::new(|_| Config::integer("N").load_current()),
      Arc::new(MapConfigProvider::from_pairs([("N", "7")])),
    );
    assert_eq!(run_blocking(eff, ()).unwrap(), 7);
  }

  #[test]
  fn current_config_provider_none_outside_scope() {
    assert!(current_config_provider().is_none());
  }
}
