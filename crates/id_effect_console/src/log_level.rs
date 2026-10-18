//! `/_console/api/log-level`: the tracing `EnvFilter` (through a reload handle) and the
//! process-wide `EffectLogger` minimum level, read and changed at runtime.

use id_effect_logger::{LogLevel, global_minimum_log_level, set_global_minimum_log_level};
use serde::{Deserialize, Serialize};
use tracing_subscriber::{EnvFilter, Registry, reload};

/// The reload handle of an [`EnvFilter`] layered directly on a [`Registry`]: what
/// `id_effect_opentelemetry`'s `OtelStarterGuard::env_filter` returns, or what
/// `tracing_subscriber::reload::Layer::new(filter)` gives an app that builds its own subscriber.
pub type EnvFilterHandle = reload::Handle<EnvFilter, Registry>;

/// The log level in force, as `GET /_console/api/log-level` answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLevelView {
  /// The directive of the reloadable `EnvFilter` (`info,my_app=debug`); `None` when the console
  /// was not given an [`EnvFilterHandle`] (or its subscriber is gone).
  pub filter: Option<String>,
  /// The process-wide `EffectLogger` minimum (`trace` … `fatal`, `none`); `None` when unset, so
  /// each fiber's own minimum applies.
  pub logger: Option<String>,
}

/// The body of `PUT /_console/api/log-level`: at least one field.
///
/// - `level` (`trace`, `debug`, `info`, `warn`, `error`, `fatal`, `none`) sets the process-wide
///   `EffectLogger` minimum and, unless `filter` is given, the `EnvFilter` to the same level
///   (`fatal` is `error`, `none` is `off`);
/// - `filter` sets the `EnvFilter` to a full directive and needs an [`EnvFilterHandle`].
///
/// Both are validated before either changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLevelChange {
  /// The level for both the `EffectLogger` and (without `filter`) the `EnvFilter`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub level: Option<String>,
  /// A full `EnvFilter` directive.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub filter: Option<String>,
}

/// Why a [`LogLevelChange`] was not applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LogLevelError {
  /// The change does not parse or is empty (`400`).
  Invalid(String),
  /// The subscriber behind the handle is gone (`500`).
  Reload(String),
}

/// The `EnvFilter` directive equivalent to the minimum `level`.
fn directive_for(level: LogLevel) -> &'static str {
  match level {
    LogLevel::Trace => "trace",
    LogLevel::Debug => "debug",
    LogLevel::Info => "info",
    LogLevel::Warn => "warn",
    LogLevel::Error | LogLevel::Fatal => "error",
    LogLevel::None => "off",
  }
}

pub(crate) fn current(filter: Option<&EnvFilterHandle>) -> LogLevelView {
  LogLevelView {
    filter: filter.and_then(|h| h.with_current(ToString::to_string).ok()),
    logger: global_minimum_log_level().map(|l| l.as_str().to_ascii_lowercase()),
  }
}

pub(crate) fn apply(
  filter: Option<&EnvFilterHandle>,
  change: LogLevelChange,
) -> Result<LogLevelView, LogLevelError> {
  if change.level.is_none() && change.filter.is_none() {
    return Err(LogLevelError::Invalid(
      "expected `level` and/or `filter`".to_owned(),
    ));
  }
  let level = change
    .level
    .as_deref()
    .map(str::parse::<LogLevel>)
    .transpose()
    .map_err(LogLevelError::Invalid)?;
  let directive = match (&change.filter, level) {
    (Some(directive), _) => Some(directive.clone()),
    (None, Some(level)) => Some(directive_for(level).to_owned()),
    (None, None) => None,
  };
  let reload = match (filter, directive) {
    (Some(handle), Some(directive)) => {
      let parsed = EnvFilter::try_new(&directive)
        .map_err(|e| LogLevelError::Invalid(format!("invalid filter {directive:?}: {e}")))?;
      Some((handle, parsed))
    }
    (None, Some(_)) if change.filter.is_some() => {
      return Err(LogLevelError::Invalid(
        "no reloadable EnvFilter is installed; only `level` can change".to_owned(),
      ));
    }
    _ => None,
  };
  if let Some((handle, parsed)) = reload {
    handle
      .reload(parsed)
      .map_err(|e| LogLevelError::Reload(e.to_string()))?;
  }
  if let Some(level) = level {
    set_global_minimum_log_level(Some(level));
  }
  Ok(current(filter))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn change(level: Option<&str>, filter: Option<&str>) -> LogLevelChange {
    LogLevelChange {
      level: level.map(str::to_owned),
      filter: filter.map(str::to_owned),
    }
  }

  #[test]
  fn levels_map_to_filter_directives() {
    assert_eq!(directive_for(LogLevel::Fatal), "error");
    assert_eq!(directive_for(LogLevel::None), "off");
    assert_eq!(directive_for(LogLevel::Debug), "debug");
  }

  #[test]
  fn invalid_changes_are_rejected_before_anything_changes() {
    let (_layer, handle) = reload::Layer::<EnvFilter, Registry>::new(EnvFilter::new("info"));
    for bad in [
      change(None, None),
      change(Some("loud"), None),
      change(Some("debug"), Some("[[[")),
    ] {
      assert!(matches!(
        apply(Some(&handle), bad),
        Err(LogLevelError::Invalid(_))
      ));
    }
    assert_eq!(current(Some(&handle)).filter.as_deref(), Some("info"));
  }

  #[test]
  fn a_filter_needs_a_handle_and_a_live_subscriber() {
    assert!(matches!(
      apply(None, change(None, Some("info"))),
      Err(LogLevelError::Invalid(_))
    ));
    let (layer, handle) = reload::Layer::<EnvFilter, Registry>::new(EnvFilter::new("info"));
    drop(layer);
    assert_eq!(current(Some(&handle)).filter, None);
    assert!(matches!(
      apply(Some(&handle), change(None, Some("debug"))),
      Err(LogLevelError::Reload(_))
    ));
  }
}
