//! Low-level reads against the injected [`crate::ConfigProviderService`].
//!
//! Every public function returns a `next` [`Effect<A, E, R>`](Effect) where
//! `R: Selector<ConfigProviderService, I>` (the capability is in `R`; the index `I` is inferred).
//! The provider is read synchronously from the environment with
//! [`need`](::id_effect::next::need), so every effect is a fused synchronous step.

use ::id_effect::next::{Effect, Selector, fail, need, succeed};

use crate::error::ConfigError;
use crate::provider::{ConfigProvider, ConfigProviderService};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Runs `f` against the [`ConfigProviderService`] in the environment.
pub(crate) fn with_provider<A, E, R, I>(
  f: impl FnOnce(&dyn ConfigProvider) -> Result<A, E> + Send + 'static,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  need::<ConfigProviderService>().flat_map(move |service| match f(service.0.as_ref()) {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  })
}

/// `Config.withDefault` as a method — only [`ConfigError::Missing`] is swapped.
pub trait WithConfigDefault<A, R>: Sized {
  /// Return `default` in place of the effect value when the error is [`ConfigError::Missing`].
  fn with_default(self, default: A) -> Effect<A, ConfigError, R>;
}

impl<A, R> WithConfigDefault<A, R> for Effect<A, ConfigError, R>
where
  A: Send + 'static,
  R: Send + Sync + 'static,
{
  fn with_default(self, default: A) -> Effect<A, ConfigError, R> {
    self.catch(move |e| match e {
      ConfigError::Missing { .. } => succeed(default),
      other => fail(other),
    })
  }
}

// ── path helpers ──────────────────────────────────────────────────────────────

/// Build a multi-segment path, e.g. `nested_path("SERVER", &["HOST"])` → `["SERVER", "HOST"]`.
#[inline]
pub fn nested_path(namespace: &str, leaf: &[&str]) -> Vec<String> {
  std::iter::once(namespace.to_string())
    .chain(leaf.iter().map(|s| (*s).to_string()))
    .collect()
}

fn owned(path: &[&str]) -> Vec<String> {
  path.iter().map(|s| s.to_string()).collect()
}

// ── primitive reads ───────────────────────────────────────────────────────────

/// Required string at `path` (shared by [`read_string`] and [`read_nested_string`]).
fn read_string_at<A, E, R, I>(path_owned: Vec<String>) -> Effect<A, E, R>
where
  A: From<String> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  with_provider(move |provider| {
    let refs: Vec<&str> = path_owned.iter().map(String::as_str).collect();
    let path_str = refs.join(".");
    match provider.load_raw(&refs) {
      Err(e) => Err(E::from(e)),
      Ok(None) => Err(E::from(ConfigError::Missing { path: path_str })),
      Ok(Some(s)) => Ok(A::from(s)),
    }
  })
}

/// String list at `path` (shared by [`read_string_list`] and [`read_nested_string_list`]).
fn read_string_list_at<A, E, R, I>(path_owned: Vec<String>) -> Effect<A, E, R>
where
  A: From<Vec<String>> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  with_provider(move |provider| {
    let refs: Vec<&str> = path_owned.iter().map(String::as_str).collect();
    let path_str = refs.join(".");
    let s = match provider.load_raw(&refs) {
      Err(e) => return Err(E::from(e)),
      Ok(None) => return Err(E::from(ConfigError::Missing { path: path_str })),
      Ok(Some(s)) => s,
    };
    let delim = provider.seq_delim();
    let list: Vec<String> = s
      .split(delim)
      .map(str::trim)
      .filter(|x| !x.is_empty())
      .map(str::to_string)
      .collect();
    Ok(A::from(list))
  })
}

/// Required scalar at `path`, parsed by `parse` (missing → [`ConfigError::Missing`]).
fn read_parsed<T, A, E, R, I>(
  path: &[&str],
  parse: impl FnOnce(&str, String) -> Result<T, ConfigError> + Send + 'static,
) -> Effect<A, E, R>
where
  A: From<T> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  let path_owned = owned(path);
  with_provider(move |provider| {
    let refs: Vec<&str> = path_owned.iter().map(String::as_str).collect();
    let path_str = refs.join(".");
    let s = match provider.load_raw(&refs) {
      Err(e) => return Err(E::from(e)),
      Ok(None) => return Err(E::from(ConfigError::Missing { path: path_str })),
      Ok(Some(s)) => s,
    };
    parse(&path_str, s).map(A::from).map_err(E::from)
  })
}

/// Required string.
pub fn read_string<A, E, R, I>(path: &[&str]) -> Effect<A, E, R>
where
  A: From<String> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  read_string_at(owned(path))
}

/// Optional string — missing key yields `None`.
pub fn read_string_opt<A, E, R, I>(path: &[&str]) -> Effect<A, E, R>
where
  A: From<Option<String>> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  let path_owned = owned(path);
  with_provider(move |provider| {
    let refs: Vec<&str> = path_owned.iter().map(String::as_str).collect();
    match provider.load_raw(&refs) {
      Err(e) => Err(E::from(e)),
      Ok(raw) => Ok(A::from(raw)),
    }
  })
}

/// Floating-point number parsed from a string scalar.
pub fn read_number<A, E, R, I>(path: &[&str]) -> Effect<A, E, R>
where
  A: From<f64> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  read_parsed(path, |path_str, s| {
    s.parse::<f64>().map_err(|e| ConfigError::Invalid {
      path: path_str.to_string(),
      value: s,
      reason: e.to_string(),
    })
  })
}

/// Signed 64-bit integer parsed from a string scalar.
pub fn read_i64<A, E, R, I>(path: &[&str]) -> Effect<A, E, R>
where
  A: From<i64> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  read_parsed(path, |path_str, s| {
    s.parse::<i64>().map_err(|e| ConfigError::Invalid {
      path: path_str.to_string(),
      value: s,
      reason: e.to_string(),
    })
  })
}

/// Boolean parsed from `"true"` / `"false"` / `"1"` / `"0"` / `"yes"` / `"no"`.
pub fn read_bool<A, E, R, I>(path: &[&str]) -> Effect<A, E, R>
where
  A: From<bool> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  read_parsed(path, |path_str, s| match s.to_ascii_lowercase().as_str() {
    "true" | "1" | "yes" => Ok(true),
    "false" | "0" | "no" => Ok(false),
    _ => Err(ConfigError::Invalid {
      path: path_str.to_string(),
      value: s,
      reason: "expected boolean string".into(),
    }),
  })
}

/// Sequence of strings split by [`ConfigProvider::seq_delim`](crate::provider::ConfigProvider::seq_delim).
pub fn read_string_list<A, E, R, I>(path: &[&str]) -> Effect<A, E, R>
where
  A: From<Vec<String>> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  read_string_list_at(owned(path))
}

// ── nested convenience ────────────────────────────────────────────────────────

/// [`nested_path`] then [`read_string`].
pub fn read_nested_string<A, E, R, I>(namespace: &str, leaf: &[&str]) -> Effect<A, E, R>
where
  A: From<String> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  read_string_at(nested_path(namespace, leaf))
}

/// [`nested_path`] then [`read_string_list`].
pub fn read_nested_string_list<A, E, R, I>(namespace: &str, leaf: &[&str]) -> Effect<A, E, R>
where
  A: From<Vec<String>> + Send + 'static,
  E: From<ConfigError> + Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  read_string_list_at(nested_path(namespace, leaf))
}
#[cfg(test)]
mod tests {
  use super::*;
  use crate::ConfigEnv;
  use crate::MapConfigProvider;
  use crate::ProviderOptions;
  use crate::config_env;
  use crate::test_support::run_blocking;

  fn env_map(pairs: &[(&str, &str)]) -> ConfigEnv {
    config_env(MapConfigProvider::from_pairs(pairs.iter().copied()))
  }

  #[test]
  fn nested_path_builds_segments() {
    assert_eq!(
      nested_path("SERVER", &["HOST", "PORT"]),
      vec!["SERVER", "HOST", "PORT"]
    );
  }

  #[test]
  fn read_string_ok_and_missing() {
    let v: String = run_blocking(
      read_string::<String, ConfigError, _, _>(&["K"]),
      env_map(&[("K", "hello")]),
    )
    .unwrap();
    assert_eq!(v, "hello");

    let err = run_blocking(
      read_string::<String, ConfigError, _, _>(&["MISSING"]),
      env_map(&[]),
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Missing { .. }));
  }

  #[test]
  fn read_string_opt_some_and_none() {
    let v: Option<String> = run_blocking(
      read_string_opt::<Option<String>, ConfigError, _, _>(&["K"]),
      env_map(&[("K", "x")]),
    )
    .unwrap();
    assert_eq!(v, Some("x".into()));

    let none: Option<String> = run_blocking(
      read_string_opt::<Option<String>, ConfigError, _, _>(&["MISSING"]),
      env_map(&[]),
    )
    .unwrap();
    assert_eq!(none, None);
  }

  #[test]
  fn read_number_ok_missing_invalid() {
    let n: f64 = run_blocking(
      read_number::<f64, ConfigError, _, _>(&["N"]),
      env_map(&[("N", "3.5")]),
    )
    .unwrap();
    assert!((n - 3.5).abs() < f64::EPSILON);

    let err = run_blocking(
      read_number::<f64, ConfigError, _, _>(&["MISSING"]),
      env_map(&[]),
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Missing { .. }));

    let err = run_blocking(
      read_number::<f64, ConfigError, _, _>(&["N"]),
      env_map(&[("N", "not-a-number")]),
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  #[test]
  fn read_i64_ok_missing_invalid() {
    let n: i64 = run_blocking(
      read_i64::<i64, ConfigError, _, _>(&["N"]),
      env_map(&[("N", "-42")]),
    )
    .unwrap();
    assert_eq!(n, -42);

    let err = run_blocking(
      read_i64::<i64, ConfigError, _, _>(&["MISSING"]),
      env_map(&[]),
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Missing { .. }));

    let err = run_blocking(
      read_i64::<i64, ConfigError, _, _>(&["N"]),
      env_map(&[("N", "1.2")]),
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  #[test]
  fn read_bool_variants_and_invalid() {
    for (raw, expected) in [
      ("true", true),
      ("TRUE", true),
      ("1", true),
      ("yes", true),
      ("false", false),
      ("0", false),
      ("no", false),
    ] {
      let b: bool = run_blocking(
        read_bool::<bool, ConfigError, _, _>(&["B"]),
        env_map(&[("B", raw)]),
      )
      .unwrap();
      assert_eq!(b, expected, "raw={raw}");
    }

    let err = run_blocking(
      read_bool::<bool, ConfigError, _, _>(&["MISSING"]),
      env_map(&[]),
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Missing { .. }));

    let err = run_blocking(
      read_bool::<bool, ConfigError, _, _>(&["B"]),
      env_map(&[("B", "maybe")]),
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  #[test]
  fn read_string_list_splits_on_seq_delim() {
    let mut m = std::collections::HashMap::new();
    m.insert("TAGS".into(), "a, b ,c".into());
    let p = MapConfigProvider::with_options(
      m,
      ProviderOptions {
        path_delim: "_",
        seq_delim: ",",
      },
    );
    let env = config_env(p);
    let tags: Vec<String> = run_blocking(
      read_string_list::<Vec<String>, ConfigError, _, _>(&["TAGS"]),
      env,
    )
    .unwrap();
    assert_eq!(tags, vec!["a", "b", "c"]);
  }

  #[test]
  fn read_nested_string_and_list() {
    let v: String = run_blocking(
      read_nested_string::<String, ConfigError, _, _>("SERVER", &["HOST"]),
      env_map(&[("SERVER_HOST", "z")]),
    )
    .unwrap();
    assert_eq!(v, "z");

    let list: Vec<String> = run_blocking(
      read_nested_string_list::<Vec<String>, ConfigError, _, _>("APP", &["IDS"]),
      env_map(&[("APP_IDS", "1,2")]),
    )
    .unwrap();
    assert_eq!(list, vec!["1", "2"]);
  }

  #[test]
  fn read_with_empty_path_is_invalid() {
    let err =
      run_blocking(read_string::<String, ConfigError, _, _>(&[]), env_map(&[])).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  #[test]
  fn with_default_replaces_only_missing() {
    let def: String = run_blocking(
      read_string::<String, ConfigError, _, _>(&["K"]).with_default("default".into()),
      env_map(&[]),
    )
    .unwrap();
    assert_eq!(def, "default");

    let v: String = run_blocking(
      read_string::<String, ConfigError, _, _>(&["K"]).with_default("default".into()),
      env_map(&[("K", "real")]),
    )
    .unwrap();
    assert_eq!(v, "real");

    let err = run_blocking(
      read_number::<f64, ConfigError, _, _>(&["N"]).with_default(0.0),
      env_map(&[("N", "bad")]),
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }
}
