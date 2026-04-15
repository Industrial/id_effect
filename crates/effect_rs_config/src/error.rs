//! Unified errors for Figment extraction and [`crate::ConfigProvider`] reads.

/// Configuration load / parse failure (Figment + Effect.ts-style provider reads).
#[derive(Debug)]
#[non_exhaustive]
pub enum ConfigError {
  /// Figment merge / deserialize failure (see [`extract`](crate::extract)).
  Figment(
    /// Underlying Figment error.
    figment::Error,
  ),
  /// No value at the flattened lookup key (Effect `ConfigError.MissingData`).
  Missing {
    /// Flattened lookup path where no value was found.
    path: String,
  },
  /// Value present but not usable for the requested type (Effect `ConfigError.InvalidData`).
  Invalid {
    /// Dot- or delimiter-joined path for the invalid value.
    path: String,
    /// Raw string form of the value that could not be coerced.
    value: String,
    /// Parser or validation explanation.
    reason: String,
  },
  /// Environment variable is not valid UTF-8.
  InvalidUtf8 {
    /// Name of the environment variable that was not valid UTF-8.
    var: String,
  },
}

impl From<figment::Error> for ConfigError {
  fn from(value: figment::Error) -> Self {
    Self::Figment(value)
  }
}

impl From<core::convert::Infallible> for ConfigError {
  fn from(e: core::convert::Infallible) -> Self {
    match e {}
  }
}

/// Owned, `'static` summary for [`ConfigError`] display via [`effect_rs::Matcher`] (avoids matching on
/// `&ConfigError`, which is not `'static`).
#[derive(Clone)]
enum ConfigErrorDisp {
  Figment(String),
  Missing {
    path: String,
  },
  Invalid {
    path: String,
    value: String,
    reason: String,
  },
  InvalidUtf8 {
    var: String,
  },
}

impl From<&ConfigError> for ConfigErrorDisp {
  fn from(e: &ConfigError) -> Self {
    match e {
      ConfigError::Figment(err) => Self::Figment(format!("{err}")),
      ConfigError::Missing { path } => Self::Missing { path: path.clone() },
      ConfigError::Invalid {
        path,
        value,
        reason,
      } => Self::Invalid {
        path: path.clone(),
        value: value.clone(),
        reason: reason.clone(),
      },
      ConfigError::InvalidUtf8 { var } => Self::InvalidUtf8 { var: var.clone() },
    }
  }
}

impl std::fmt::Display for ConfigError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    use effect_rs::Matcher;

    let d = ConfigErrorDisp::from(self);
    let s = Matcher::<ConfigErrorDisp, String>::new()
      .when(
        Box::new(|c: &ConfigErrorDisp| matches!(c, ConfigErrorDisp::Figment(_))),
        |c| match c {
          ConfigErrorDisp::Figment(msg) => msg,
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &ConfigErrorDisp| matches!(c, ConfigErrorDisp::Missing { .. })),
        |c| match c {
          ConfigErrorDisp::Missing { path } => format!("missing configuration at path {path:?}"),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &ConfigErrorDisp| matches!(c, ConfigErrorDisp::Invalid { .. })),
        |c| match c {
          ConfigErrorDisp::Invalid {
            path,
            value,
            reason,
          } => format!("invalid configuration at path {path:?} (value={value:?}): {reason}"),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &ConfigErrorDisp| matches!(c, ConfigErrorDisp::InvalidUtf8 { .. })),
        |c| match c {
          ConfigErrorDisp::InvalidUtf8 { var } => {
            format!("environment variable {var:?} is not valid UTF-8")
          }
          _ => unreachable!(),
        },
      )
      .run_exhaustive(d);
    f.write_str(&s)
  }
}

impl std::error::Error for ConfigError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      ConfigError::Figment(e) => Some(e),
      _ => None,
    }
  }
}

#[cfg(test)]
mod tests {
  use std::error::Error;
  use super::*;

  #[test]
  fn config_error_matcher_all_variants_covered() {
    use effect_rs::Matcher;

    let fig_display =
      ConfigError::from(figment::Figment::new().extract::<i32>().unwrap_err()).to_string();
    let fig_for_match = ConfigError::from(figment::Figment::new().extract::<i32>().unwrap_err());
    let via_matcher = Matcher::<ConfigError, String>::new()
      .when(
        Box::new(|c: &ConfigError| matches!(c, ConfigError::Figment(_))),
        |c: ConfigError| match c {
          ConfigError::Figment(e) => format!("{e}"),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &ConfigError| matches!(c, ConfigError::Missing { .. })),
        |c: ConfigError| match c {
          ConfigError::Missing { path } => format!("missing configuration at path {path:?}"),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &ConfigError| matches!(c, ConfigError::Invalid { .. })),
        |c: ConfigError| match c {
          ConfigError::Invalid {
            path,
            value,
            reason,
          } => format!("invalid configuration at path {path:?} (value={value:?}): {reason}"),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &ConfigError| matches!(c, ConfigError::InvalidUtf8 { .. })),
        |c: ConfigError| match c {
          ConfigError::InvalidUtf8 { var } => {
            format!("environment variable {var:?} is not valid UTF-8")
          }
          _ => unreachable!(),
        },
      )
      .or_else(|c| format!("configuration error: {c:?}"))
      .run_exhaustive(fig_for_match);
    assert!(!via_matcher.is_empty());
    assert_eq!(via_matcher, fig_display);

    let missing = ConfigError::Missing {
      path: "db.url".into(),
    };
    assert_eq!(
      missing.to_string(),
      "missing configuration at path \"db.url\""
    );

    let invalid = ConfigError::Invalid {
      path: "port".into(),
      value: "x".into(),
      reason: "not a number".into(),
    };
    assert_eq!(
      invalid.to_string(),
      "invalid configuration at path \"port\" (value=\"x\"): not a number"
    );

    let utf8 = ConfigError::InvalidUtf8 {
      var: "WEIRD".into(),
    };
    assert_eq!(
      utf8.to_string(),
      "environment variable \"WEIRD\" is not valid UTF-8"
    );
  }

  #[test]
  fn config_error_source_figment_variant_has_source() {
    let fig_err = figment::Figment::new().extract::<i32>().unwrap_err();
    let e = ConfigError::Figment(fig_err);
    assert!(e.source().is_some());
  }

  #[test]
  fn config_error_source_non_figment_variants_return_none() {
    let missing = ConfigError::Missing {
      path: "x".into(),
    };
    assert!(missing.source().is_none());

    let invalid = ConfigError::Invalid {
      path: "x".into(),
      value: "v".into(),
      reason: "r".into(),
    };
    assert!(invalid.source().is_none());

    let utf8 = ConfigError::InvalidUtf8 {
      var: "VAR".into(),
    };
    assert!(utf8.source().is_none());
  }
}
