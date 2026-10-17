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
  /// The selected configuration profile is not one the application declared (see
  /// `Profiles::known`, features `toml` + `env`).
  UnknownProfile {
    /// Profile name that was requested (flag, environment variable or default).
    profile: String,
    /// Profiles the application declared, in declaration order.
    known: Vec<String>,
  },
  /// A HashiCorp Vault secret could not be read (feature `vault`). The reason never contains a
  /// secret value or the token.
  Vault {
    /// Secret path below the mount (`ledger/db`) whose read failed.
    path: String,
    /// What failed: the transport, an HTTP status, or the response shape.
    kind: VaultErrorKind,
    /// Vault's `errors` list, the transport error, or the decoding problem.
    reason: String,
  },
  /// Several independent failures, every one reported (Effect `ConfigError.And`).
  ///
  /// Produced by [`Config::all`](crate::Config::all) and the combinators built on it
  /// ([`Config::zip`](crate::Config::zip), [`Config::zip_with`](crate::Config::zip_with)) when
  /// more than one component fails. Always flat (no nested `And`) and holds at least two errors;
  /// build one with [`ConfigError::and`].
  And(
    /// The failures, in the order of the components.
    Vec<ConfigError>,
  ),
}

/// Shown in place of a secret's raw value in errors.
pub(crate) const REDACTED: &str = "<redacted>";

impl ConfigError {
  /// The same error with every raw value removed, for errors raised while loading a secret.
  ///
  /// `Invalid` keeps its path and reason but drops the value. A Figment error can quote the
  /// offending value in its message, so it becomes `Invalid` at the same path with a generic
  /// reason. `And` is redacted element by element.
  pub(crate) fn redact_values(self) -> Self {
    match self {
      Self::Invalid { path, reason, .. } => Self::Invalid {
        path,
        value: REDACTED.to_string(),
        reason,
      },
      Self::Figment(e) => Self::Invalid {
        path: e.path.join("."),
        value: REDACTED.to_string(),
        reason: "value has the wrong type or format (details hidden for a secret)".to_string(),
      },
      Self::And(errors) => Self::And(errors.into_iter().map(Self::redact_values).collect()),
      other => other,
    }
  }
}

/// The failing step of a [`ConfigError::Vault`] read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VaultErrorKind {
  /// The request did not complete (connection refused, TLS, timeout).
  Transport,
  /// Vault answered with a non-success HTTP status (`403` permission denied, `404` no such
  /// secret).
  Status(
    /// The HTTP status code.
    u16,
  ),
  /// The body is not a KV v2 read response (`{"data": {"data": {..}}}`).
  Decode,
}

impl std::fmt::Display for VaultErrorKind {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      VaultErrorKind::Transport => f.write_str("transport error"),
      VaultErrorKind::Status(status) => write!(f, "HTTP {status}"),
      VaultErrorKind::Decode => f.write_str("unexpected response"),
    }
  }
}

impl ConfigError {
  /// Combine failures (Effect `ConfigError.And`): nested `And`s are flattened and a single error
  /// is returned as is.
  ///
  /// # Panics
  ///
  /// When `errors` is empty (there is nothing to report).
  pub fn and(errors: impl IntoIterator<Item = ConfigError>) -> Self {
    let mut flat = Vec::new();
    for e in errors {
      match e {
        ConfigError::And(inner) => flat.extend(inner),
        other => flat.push(other),
      }
    }
    match flat.len() {
      0 => panic!("ConfigError::and needs at least one error"),
      1 => flat.pop().expect("one error"),
      _ => ConfigError::And(flat),
    }
  }

  /// Every individual failure: the components of an [`And`](ConfigError::And), or `self`.
  pub fn errors(&self) -> &[ConfigError] {
    match self {
      ConfigError::And(errors) => errors,
      other => std::slice::from_ref(other),
    }
  }

  /// `true` when every failure is [`Missing`](ConfigError::Missing) (Effect
  /// `ConfigError.isMissingDataOnly`): the condition under which
  /// [`Config::with_default`](crate::Config::with_default) and
  /// [`Config::or_else`](crate::Config::or_else) fall back.
  pub fn is_missing_only(&self) -> bool {
    self
      .errors()
      .iter()
      .all(|e| matches!(e, ConfigError::Missing { .. }))
  }
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

/// Owned, `'static` summary for [`ConfigError`] display via [`id_effect::Matcher`] (avoids matching on
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
  UnknownProfile {
    profile: String,
    known: Vec<String>,
  },
  Vault {
    path: String,
    kind: VaultErrorKind,
    reason: String,
  },
  And(Vec<String>),
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
      ConfigError::UnknownProfile { profile, known } => Self::UnknownProfile {
        profile: profile.clone(),
        known: known.clone(),
      },
      ConfigError::Vault { path, kind, reason } => Self::Vault {
        path: path.clone(),
        kind: *kind,
        reason: reason.clone(),
      },
      ConfigError::And(errors) => Self::And(errors.iter().map(ToString::to_string).collect()),
    }
  }
}

impl std::fmt::Display for ConfigError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    use id_effect::Matcher;

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
      .when(
        Box::new(|c: &ConfigErrorDisp| matches!(c, ConfigErrorDisp::UnknownProfile { .. })),
        |c| match c {
          ConfigErrorDisp::UnknownProfile { profile, known } => format!(
            "unknown configuration profile {profile:?} (known: {})",
            known.join(", ")
          ),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &ConfigErrorDisp| matches!(c, ConfigErrorDisp::Vault { .. })),
        |c| match c {
          ConfigErrorDisp::Vault { path, kind, reason } => {
            format!("vault secret {path:?}: {kind}: {reason}")
          }
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &ConfigErrorDisp| matches!(c, ConfigErrorDisp::And(_))),
        |c| match c {
          ConfigErrorDisp::And(errors) => {
            format!(
              "{} configuration errors: {}",
              errors.len(),
              errors.join("; ")
            )
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
  use super::*;
  use std::error::Error;

  #[test]
  fn redact_values_hides_raw_values_and_keeps_paths() {
    let mut fig = figment::Error::from("invalid type: found string \"s3cr3t\"".to_string());
    fig.path = vec!["db".to_string(), "password".to_string()];
    let err = ConfigError::Figment(fig).redact_values();
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains("s3cr3t"), "{shown}");
    assert!(
      matches!(&err, ConfigError::Invalid { path, value, .. } if path == "db.password" && value == REDACTED)
    );

    let missing = ConfigError::Missing {
      path: "KEY".to_string(),
    }
    .redact_values();
    assert!(matches!(missing, ConfigError::Missing { ref path } if path == "KEY"));
  }

  #[test]
  fn config_error_matcher_all_variants_covered() {
    use id_effect::Matcher;

    let fig_display =
      ConfigError::from(figment::Figment::new().extract::<i32>().unwrap_err()).to_string();
    let fig_for_match = ConfigError::from(figment::Figment::new().extract::<i32>().unwrap_err());
    let render = |err: ConfigError| {
      Matcher::<ConfigError, String>::new()
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
        .run_exhaustive(err)
    };
    let via_matcher = render(fig_for_match);
    assert!(!via_matcher.is_empty());
    assert_eq!(via_matcher, fig_display);

    // A user-side Matcher over every variant agrees with `Display`.
    let variants = || {
      [
        ConfigError::Missing { path: "a".into() },
        ConfigError::Invalid {
          path: "b".into(),
          value: "v".into(),
          reason: "r".into(),
        },
        ConfigError::InvalidUtf8 { var: "C".into() },
      ]
    };
    for (err, same) in variants().into_iter().zip(variants()) {
      assert_eq!(render(err), same.to_string());
    }

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
    let missing = ConfigError::Missing { path: "x".into() };
    assert!(missing.source().is_none());

    let invalid = ConfigError::Invalid {
      path: "x".into(),
      value: "v".into(),
      reason: "r".into(),
    };
    assert!(invalid.source().is_none());

    let utf8 = ConfigError::InvalidUtf8 { var: "VAR".into() };
    assert!(utf8.source().is_none());

    let unknown = ConfigError::UnknownProfile {
      profile: "staging".into(),
      known: vec!["dev".into()],
    };
    assert!(unknown.source().is_none());
  }

  #[test]
  fn config_error_unknown_profile_display_lists_known_profiles() {
    let err = ConfigError::UnknownProfile {
      profile: "staging".into(),
      known: vec!["dev".into(), "test".into(), "prod".into()],
    };
    assert_eq!(
      err.to_string(),
      "unknown configuration profile \"staging\" (known: dev, test, prod)"
    );
    assert!(matches!(
      ConfigErrorDisp::from(&err),
      ConfigErrorDisp::UnknownProfile { .. }
    ));
  }

  #[test]
  fn config_error_vault_display_names_path_and_kind() {
    let render = |kind| {
      ConfigError::Vault {
        path: "ledger/db".into(),
        kind,
        reason: "permission denied".into(),
      }
      .to_string()
    };
    assert_eq!(
      render(VaultErrorKind::Status(403)),
      "vault secret \"ledger/db\": HTTP 403: permission denied"
    );
    assert!(render(VaultErrorKind::Transport).contains(": transport error: "));
    assert!(render(VaultErrorKind::Decode).contains(": unexpected response: "));
    let err = ConfigError::Vault {
      path: "p".into(),
      kind: VaultErrorKind::Decode,
      reason: "r".into(),
    };
    assert!(err.source().is_none());
    assert!(!err.is_missing_only());
  }

  #[test]
  fn config_error_disp_from_all_variants() {
    let fig = ConfigError::Figment(figment::Figment::new().extract::<i32>().unwrap_err());
    let missing = ConfigError::Missing { path: "p".into() };
    let invalid = ConfigError::Invalid {
      path: "p".into(),
      value: "v".into(),
      reason: "r".into(),
    };
    let utf8 = ConfigError::InvalidUtf8 { var: "V".into() };
    for err in [fig, missing, invalid, utf8] {
      let _disp = ConfigErrorDisp::from(&err);
      let rendered = err.to_string();
      assert!(!rendered.is_empty());
    }
  }
}
