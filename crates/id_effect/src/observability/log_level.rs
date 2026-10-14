//! [`LogLevel`] — severity levels shared by logging backends and configuration.
//!
//! Lives in core (moved from `id_effect_logger` in 0.6) so configuration can parse a level
//! without depending on a logging backend.

/// Logging severity level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LogLevel {
  /// Most verbose; diagnostic detail.
  Trace = 0,
  /// Development diagnostics.
  Debug = 1,
  /// Normal operational messages.
  Info = 2,
  /// Something unexpected but recoverable.
  Warn = 3,
  /// Failure or serious problem.
  Error = 4,
  /// Highest severity; mapped to `tracing::error!` with a distinct target in composite pipelines.
  Fatal = 5,
  /// Use as a **minimum** level only: no messages pass the filter. Do not emit log lines at this level.
  None = 255,
}

impl LogLevel {
  /// Numeric severity (higher = more severe). Used for minimum-level filtering.
  #[inline]
  pub const fn severity(self) -> u8 {
    match self {
      LogLevel::None => 255,
      _ => self as u8,
    }
  }

  /// Whether a log at `message_level` should be emitted when `self` is the configured minimum.
  #[inline]
  pub const fn allows(self, message_level: LogLevel) -> bool {
    match (self, message_level) {
      (LogLevel::None, _) | (_, LogLevel::None) => false,
      _ => message_level.severity() >= self.severity(),
    }
  }

  /// Stable uppercase name for structured / JSON backends.
  #[inline]
  pub const fn as_str(self) -> &'static str {
    match self {
      LogLevel::Trace => "TRACE",
      LogLevel::Debug => "DEBUG",
      LogLevel::Info => "INFO",
      LogLevel::Warn => "WARN",
      LogLevel::Error => "ERROR",
      LogLevel::Fatal => "FATAL",
      LogLevel::None => "NONE",
    }
  }
}

impl std::str::FromStr for LogLevel {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s.trim().to_ascii_lowercase().as_str() {
      "trace" => Ok(LogLevel::Trace),
      "debug" => Ok(LogLevel::Debug),
      "info" => Ok(LogLevel::Info),
      "warn" | "warning" => Ok(LogLevel::Warn),
      "error" => Ok(LogLevel::Error),
      "fatal" => Ok(LogLevel::Fatal),
      "none" => Ok(LogLevel::None),
      other => Err(format!("unknown log level: {other:?}")),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::str::FromStr;

  #[test]
  fn minimum_level_filters_by_severity() {
    assert!(LogLevel::Info.allows(LogLevel::Warn));
    assert!(LogLevel::Info.allows(LogLevel::Info));
    assert!(!LogLevel::Info.allows(LogLevel::Debug));
    assert!(!LogLevel::None.allows(LogLevel::Fatal));
    assert!(!LogLevel::Trace.allows(LogLevel::None));
  }

  #[test]
  fn parses_case_insensitively_with_warning_alias() {
    assert_eq!(LogLevel::from_str(" WARNING ").unwrap(), LogLevel::Warn);
    assert_eq!(LogLevel::from_str("fatal").unwrap(), LogLevel::Fatal);
    assert!(LogLevel::from_str("loud").is_err());
  }

  #[test]
  fn names_and_severity_are_stable() {
    assert_eq!(LogLevel::Error.as_str(), "ERROR");
    assert_eq!(LogLevel::None.severity(), 255);
    assert_eq!(LogLevel::Trace.severity(), 0);
  }
}
