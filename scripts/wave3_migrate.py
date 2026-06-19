#!/usr/bin/env python3
"""Wave 3 migration script for id_effect DI maturity."""
from pathlib import Path
import re

ROOT = Path("/home/tom/Code/rust/id_effect")

def write(path, content):
    path.write_text(content)
    print(f"  wrote {path.relative_to(ROOT)}")

def migrate_logger_lib():
    path = ROOT / "crates/id_effect_logger/src/lib.rs"
    text = path.read_text()
    text = text.replace(
        "use ::id_effect::{BoxFuture, Effect, EffectHashMap, FiberRef, IntoBind, Needs, box_future};",
        "use ::id_effect::{Effect, EffectHashMap, FiberRef, Needs};",
    )
    old_keys = """#[allow(missing_docs)]
mod effect_log_keys {
  use super::{EffectLogger, FiberRef, LogLevel};
  id_effect::define_capability!(EffectLogKey, EffectLogger);
  id_effect::define_capability!(EffectLogMinLevelKey, FiberRef<LogLevel>);
}
pub use effect_log_keys::{EffectLogKey, EffectLogMinLevelKey};"""
    new_keys = """/// Fiber-local minimum log level installed by [`provide_minimum_log_level`].
#[::id_effect::capability(FiberRef<LogLevel>)]
pub struct EffectLogMinLevel;

pub use self::{EffectLoggerKey, EffectLogMinLevelKey};"""
    text = text.replace(old_keys, new_keys)
    text = text.replace(
        "/// Extracted from the environment with `~EffectLogger` inside [`id_effect::effect!`].\n"
        "/// After extraction its methods return `Effect<(), EffectLoggerError, R>` and\n"
        "/// are themselves awaited with `~`.\n"
        "#[derive(Clone, Copy, Debug, Default)]\n"
        "pub struct EffectLogger;",
        "/// Extracted from the environment with `require!(EffectLoggerKey)` inside [`id_effect::effect!`].\n"
        "/// After extraction its methods return `Effect<(), EffectLoggerError, R>` and\n"
        "/// are themselves awaited with `~`.\n"
        "#[::id_effect::capability(EffectLogger)]\n"
        "#[derive(Clone, Copy, Debug, Default)]\n"
        "pub struct EffectLogger;",
    )
    into_bind = """// ---------------------------------------------------------------------------
// Service extraction: `~EffectLogger` inside `effect!`
// ---------------------------------------------------------------------------

/// Implementing [`IntoBind`] for [`EffectLogger`] makes `~EffectLogger` valid
/// inside any `effect!` whose environment `R` holds an `EffectLogger` under
/// [`EffectLogKey`].  The zero-sized struct acts as its own "request token":
/// passing it to `~` copies the concrete value out of `R` and binds it as a
/// local variable.
impl<'a, R> IntoBind<'a, R, EffectLogger, EffectLoggerError> for EffectLogger
where
  R: Needs<EffectLogKey> + 'a,
{
  fn into_bind(self, r: &'a mut R) -> BoxFuture<'a, Result<EffectLogger, EffectLoggerError>> {
    Box::pin(ready(Ok(*r.need())))
  }
}

"""
    text = text.replace(into_bind, "")
    text = text.replace("EffectLogKey", "EffectLoggerKey")
    text = text.replace(
        "Extract the logger from the environment once with `~EffectLogger`, then call",
        "Extract the logger from the environment once with `require!(EffectLoggerKey)`, then call",
    )
    text = text.replace("let logger = ~EffectLogger;", "let logger = require!(EffectLoggerKey);")
    old_test = """  mod into_bind_extraction {
    use super::*;

    #[test]
    fn extracts_logger_copy_from_context() {
      let effect: ::id_effect::Effect<EffectLogger, EffectLoggerError, LogCtx> =
        ::id_effect::Effect::new_async(move |r| {
          Box::pin(async move { IntoBind::into_bind(EffectLogger, r).await })
        });
      let result = run_blocking(effect, test_ctx());
      assert!(result.is_ok());
    }

    #[test]
    fn extracted_logger_can_emit_log_via_run_blocking() {
      init_subscriber();
      let effect: ::id_effect::Effect<EffectLogger, EffectLoggerError, LogCtx> =
        ::id_effect::Effect::new_async(move |r| {
          Box::pin(async move { IntoBind::into_bind(EffectLogger, r).await })
        });
      let logger = run_blocking(effect, test_ctx()).expect("extraction is infallible");
      assert_eq!(run_blocking(logger.info::<()>("extracted"), ()), Ok(()));
    }
  }"""
    new_test = """  mod needs_extraction {
    use super::*;

    #[test]
    fn extracts_logger_copy_from_context() {
      let effect: ::id_effect::Effect<EffectLogger, EffectLoggerError, LogCtx> =
        ::id_effect::Effect::new(move |r: &mut LogCtx| Ok(*Needs::<EffectLoggerKey>::need(r)));
      let result = run_blocking(effect, test_ctx());
      assert!(result.is_ok());
    }

    #[test]
    fn extracted_logger_can_emit_log_via_run_blocking() {
      init_subscriber();
      let effect: ::id_effect::Effect<EffectLogger, EffectLoggerError, LogCtx> =
        ::id_effect::Effect::new(move |r: &mut LogCtx| Ok(*Needs::<EffectLoggerKey>::need(r)));
      let logger = run_blocking(effect, test_ctx()).expect("extraction is infallible");
      assert_eq!(run_blocking(logger.info::<()>("extracted"), ()), Ok(()));
    }
  }"""
    text = text.replace(old_test, new_test)
    write(path, text)

if __name__ == "__main__":
    migrate_logger_lib()
    print("partial done")
