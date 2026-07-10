//! Configuration loading in three complementary styles, aligned with
//! [Effect.ts configuration](https://effect.website/docs/configuration):
//!
//! ## 1. `Config<T>` descriptor (Effect `Config.string` / `Config.withDefault` / `Config.all`)
//!
//! The recommended approach.  Compose lazy descriptors, then evaluate with [`Config::run`]
//! (service-injected) or [`Config::load`] (direct provider reference):
//!
//! ```rust
//! use std::sync::Arc;
//! use id_effect_config::{Config, MapConfigProvider, config_env, config, ConfigError};
//! use id_effect::run_blocking;
//!
//! let p = MapConfigProvider::from_pairs([("HOST", "localhost"), ("PORT", "8080")]);
//!
//! // Descriptors — nothing is read yet
//! let host_cfg = Config::string("HOST");
//! let port_cfg = Config::integer("PORT").with_default(3000);
//!
//! // Evaluate with a direct provider (synchronous)
//! let (host, port) = config::all(host_cfg.clone(), port_cfg.clone()).load(&p).unwrap();
//! assert_eq!(host, "localhost");
//! assert_eq!(port, 8080);
//!
//! // Evaluate as Effect with service injection
//! let env = config_env(p);
//! let host2: String = run_blocking(host_cfg.run::<String, ConfigError, _>(), env.clone()).unwrap();
//! let port2: i64 = run_blocking(port_cfg.run::<i64, ConfigError, _>(), env).unwrap();
//! assert_eq!(host2, "localhost");
//! assert_eq!(port2, 8080);
//! ```
//!
//! ## 2. Figment + serde (whole-document extract)
//!
//! Build a [`Figment`](https://docs.rs/figment/latest/figment/struct.Figment.html) (layering TOML, JSON, env, …), then [`extract`] /
//! [`FigmentLayer`] — good for structured config files.
//!
//! ## 3. Low-level Effect reads via `load::read_*` with `Needs<ConfigProvider>`
//!
//! Inject the provider via the effect environment and call the free functions directly:
//!
//! ```ignore
//! use id_effect_config::{read_string, ConfigError};
//! use id_effect::Needs;
//!
//! fn load_host<A, E, R>() -> ::id_effect::Effect<A, E, R>
//! where
//!   A: From<String> + 'static,
//!   E: From<ConfigError> + 'static,
//!   R: id_effect::Needs<id_effect_config::ConfigProvider> + 'static,
//! {
//!   read_string(&["HOST"])
//! }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
mod config_desc;
mod error;
mod load;
mod provider;
mod providers;
mod secret;

pub use config_desc::{Config, all, all_vec, all3, nest, or_else as or_else_config, zip_with};
pub use error::ConfigError;
pub use load::{
  WithConfigDefault, nested_path, read_bool, read_i64, read_nested_string, read_nested_string_list,
  read_number, read_string, read_string_list, read_string_opt,
};
pub use provider::{
  ConfigProvider, ConfigProviderService, EnvConfigProvider, FigmentConfigProvider,
  MapConfigProvider, OrElseConfigProvider, ProviderOptions, ScopedConfigProvider,
};
pub use providers::{
  EnvConfigProviderLive, provide_config_provider, provide_env_config_provider,
  provide_figment_config_provider,
};
pub use secret::Secret;

// ── Service environment helper ────────────────────────────────────────────────

use std::sync::Arc;

/// Type alias for a minimal capability environment containing only [`ConfigProviderService`].
///
/// Use with [`config_env`] and `run_blocking` to evaluate `Config<T>::run()` or `read_*` effects
/// in tests and CLI entry points.
pub type ConfigEnv = ::id_effect::Env;

/// Build a minimal [`ConfigEnv`] wrapping `provider`.
///
/// ```rust
/// use id_effect_config::{Config, MapConfigProvider, config_env, ConfigError};
/// use id_effect::run_blocking;
///
/// let p = MapConfigProvider::from_pairs([("HOST", "localhost")]);
/// let host: String = run_blocking(
///   Config::string("HOST").run::<String, ConfigError, _>(),
///   config_env(p),
/// )
/// .unwrap();
/// assert_eq!(host, "localhost");
/// ```
pub fn config_env<P: ConfigProvider + 'static>(provider: P) -> ConfigEnv {
  let mut env = ::id_effect::Env::new();
  env.insert::<::id_effect::Cap<ConfigProviderService>>(ConfigProviderService(Arc::new(provider)));
  env
}

/// Mirrors `import { Config } from "effect"` — scalars, combinators, and free functions.
///
/// The *descriptor* ([`Config`]) and *free functions* ([`all`], [`zip_with`], …) are
/// re-exported here so call sites can write `config::all(…)` and `config::Config::string(…)`.
pub mod config {
  use crate::ConfigError;
  use crate::ConfigProvider;
  pub use crate::config_desc::{Config, all, all_vec, all3, nest, or_else, zip_with};
  pub use crate::secret::Secret;

  /// Required string at a single-segment path.
  #[inline]
  pub fn string(p: &dyn ConfigProvider, name: &str) -> Result<String, ConfigError> {
    Config::string(name).load(p)
  }

  /// Optional string at a single-segment path.
  #[inline]
  pub fn optional_string(
    p: &dyn ConfigProvider,
    name: &str,
  ) -> Result<Option<String>, ConfigError> {
    Config::optional_string(name).load(p)
  }

  /// Floating-point scalar.
  #[inline]
  pub fn number(p: &dyn ConfigProvider, name: &str) -> Result<f64, ConfigError> {
    Config::number(name).load(p)
  }

  /// Signed integer scalar.
  #[inline]
  pub fn integer(p: &dyn ConfigProvider, name: &str) -> Result<i64, ConfigError> {
    Config::integer(name).load(p)
  }

  /// Boolean scalar.
  #[inline]
  pub fn boolean(p: &dyn ConfigProvider, name: &str) -> Result<bool, ConfigError> {
    Config::boolean(name).load(p)
  }

  /// String under `namespace` / `name` (two path segments).
  #[inline]
  pub fn nested_string(
    p: &dyn ConfigProvider,
    namespace: &str,
    name: &str,
  ) -> Result<String, ConfigError> {
    Config::string(name).nested(namespace).load(p)
  }

  /// Optional string under `namespace` / `name`.
  #[inline]
  pub fn nested_optional_string(
    p: &dyn ConfigProvider,
    namespace: &str,
    name: &str,
  ) -> Result<Option<String>, ConfigError> {
    Config::optional_string(name).nested(namespace).load(p)
  }

  /// Floating-point under `namespace` / `name`.
  #[inline]
  pub fn nested_number(
    p: &dyn ConfigProvider,
    namespace: &str,
    name: &str,
  ) -> Result<f64, ConfigError> {
    Config::number(name).nested(namespace).load(p)
  }

  /// Signed integer under `namespace` / `name`.
  #[inline]
  pub fn nested_integer(
    p: &dyn ConfigProvider,
    namespace: &str,
    name: &str,
  ) -> Result<i64, ConfigError> {
    Config::integer(name).nested(namespace).load(p)
  }

  /// Boolean under `namespace` / `name`.
  #[inline]
  pub fn nested_boolean(
    p: &dyn ConfigProvider,
    namespace: &str,
    name: &str,
  ) -> Result<bool, ConfigError> {
    Config::boolean(name).nested(namespace).load(p)
  }

  /// Delimiter-separated string list.
  #[inline]
  pub fn string_list(p: &dyn ConfigProvider, name: &str) -> Result<Vec<String>, ConfigError> {
    Config::string_list(name).load(p)
  }

  /// String list under `namespace` / `name`.
  #[inline]
  pub fn nested_string_list(
    p: &dyn ConfigProvider,
    namespace: &str,
    name: &str,
  ) -> Result<Vec<String>, ConfigError> {
    Config::string_list(name).nested(namespace).load(p)
  }

  /// Fall back to `default` when `r` is [`ConfigError::Missing`].
  #[inline]
  pub fn with_default<T>(r: Result<T, ConfigError>, default: T) -> Result<T, ConfigError> {
    match r {
      Ok(v) => Ok(v),
      Err(ConfigError::Missing { .. }) => Ok(default),
      Err(e) => Err(e),
    }
  }
}

use std::marker::PhantomData;

use ::figment::Figment;
use serde::de::DeserializeOwned;

/// Deserialize `T` from a prepared [`Figment`].
#[inline]
pub fn extract<T: DeserializeOwned>(figment: &Figment) -> Result<T, ConfigError> {
  figment.extract().map_err(ConfigError::from)
}

/// Same as [`extract`], explicit name for boolean-heavy call sites.
#[inline]
pub fn try_extract<T: DeserializeOwned>(figment: &Figment) -> Result<T, ConfigError> {
  extract(figment)
}

/// Deserializes `T` from a shared [`Figment`] on each [`build`](Self::build).
#[derive(Debug)]
pub struct FigmentLayer<T> {
  figment: Arc<Figment>,
  _marker: PhantomData<fn() -> T>,
}

impl<T> FigmentLayer<T> {
  /// Layer that deserializes from an owned [`Figment`] on each build.
  #[inline]
  pub fn new(figment: Figment) -> Self {
    Self {
      figment: Arc::new(figment),
      _marker: PhantomData,
    }
  }

  /// Layer that reuses an existing shared [`Arc<Figment>`] on each build.
  #[inline]
  pub fn from_shared(figment: Arc<Figment>) -> Self {
    Self {
      figment,
      _marker: PhantomData,
    }
  }

  /// Borrow the merged [`Figment`] used for deserialization.
  #[inline]
  pub fn figment(&self) -> &Figment {
    self.figment.as_ref()
  }

  /// Deserialize `T` from the wrapped [`Figment`].
  #[inline]
  pub fn build(&self) -> Result<T, ConfigError>
  where
    T: DeserializeOwned,
  {
    extract(self.figment.as_ref())
  }
}

/// Common [`Figment`] builders.
pub mod figment {
  #[cfg(any(feature = "env", feature = "toml", feature = "json", feature = "yaml"))]
  use ::figment::Figment;
  #[cfg(feature = "env")]
  use ::figment::providers::Env;
  #[cfg(feature = "toml")]
  use ::figment::providers::{Format, Toml};
  #[cfg(any(feature = "toml", feature = "json", feature = "yaml"))]
  use std::path::Path;

  /// Figment containing all environment variables (no prefix filter).
  #[must_use]
  #[cfg(feature = "env")]
  pub fn from_env_raw() -> Figment {
    Figment::from(Env::raw())
  }

  /// Figment from environment variables whose names start with `prefix`.
  #[must_use]
  #[cfg(feature = "env")]
  pub fn from_env_prefixed(prefix: impl AsRef<str>) -> Figment {
    Figment::from(Env::prefixed(prefix.as_ref()))
  }

  /// Merge a TOML file into an existing [`Figment`].
  #[must_use]
  #[cfg(feature = "toml")]
  pub fn merge_toml(figment: Figment, path: impl AsRef<Path>) -> Figment {
    figment.merge(Toml::file(path))
  }

  /// New [`Figment`] consisting only of a single TOML file.
  #[must_use]
  #[cfg(feature = "toml")]
  pub fn from_toml_file(path: impl AsRef<Path>) -> Figment {
    Figment::new().merge(Toml::file(path))
  }

  /// Merge a JSON file into an existing [`Figment`].
  #[must_use]
  #[cfg(feature = "json")]
  pub fn merge_json(figment: Figment, path: impl AsRef<Path>) -> Figment {
    use ::figment::providers::{Format, Json};
    figment.merge(Json::file(path))
  }

  /// Merge a YAML file into an existing [`Figment`].
  #[must_use]
  #[cfg(feature = "yaml")]
  pub fn merge_yaml(figment: Figment, path: impl AsRef<Path>) -> Figment {
    use ::figment::providers::{Format, Yaml};
    figment.merge(Yaml::file(path))
  }

  /// Default TOML, local override TOML, then prefixed environment (common app layering).
  #[must_use]
  #[cfg(all(feature = "toml", feature = "env"))]
  pub fn layered_toml_env(
    default_toml: impl AsRef<Path>,
    local_toml: impl AsRef<Path>,
    env_prefix: impl AsRef<str>,
  ) -> Figment {
    Figment::new()
      .merge(Toml::file(default_toml))
      .merge(Toml::file(local_toml))
      .merge(Env::prefixed(env_prefix.as_ref()))
  }
}

/// Load `.env` from the current directory (fails if the file is missing or invalid).
#[cfg(feature = "dotenv")]
#[inline]
pub fn load_dotenv() -> Result<(), dotenvy::Error> {
  dotenvy::dotenv().map(|_| ())
}

/// Best-effort `.env` load; ignores missing files and parse errors.
#[cfg(feature = "dotenv")]
#[inline]
pub fn load_dotenv_optional() {
  let _ = dotenvy::dotenv();
}

#[cfg(all(test, feature = "toml"))]
mod tests {
  use super::*;
  use serde::Deserialize;
  use std::io::Write;
  use temp_env::with_var;

  #[derive(Debug, Deserialize, PartialEq)]
  struct Cfg {
    n: u32,
    s: String,
  }

  #[test]
  fn extract_from_toml_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("app.toml");
    let mut f = std::fs::File::create(&path).expect("create");
    writeln!(f, "n = 7\ns = \"hi\"").expect("write");
    drop(f);

    let fig = figment::from_toml_file(&path);
    let cfg: Cfg = extract(&fig).expect("extract");
    assert_eq!(
      cfg,
      Cfg {
        n: 7,
        s: "hi".into()
      }
    );
  }

  #[test]
  fn figment_layer_build() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.toml");
    std::fs::write(&path, "n = 1\ns = \"a\"").expect("write");

    let layer = FigmentLayer::<Cfg>::new(figment::from_toml_file(&path));
    let cfg = layer.build().expect("build");
    assert_eq!(cfg.n, 1);
    assert_eq!(cfg.s, "a");
  }

  #[test]
  fn env_provider_nested_and_default() {
    let key = "EFFECT_CONFIG_TEST_SERVER_HOST";
    with_var(key, Some("localhost"), || {
      let p = EnvConfigProvider::from_env();
      let host = config::nested_string(&p, "EFFECT_CONFIG_TEST_SERVER", "HOST").expect("host");
      assert_eq!(host, "localhost");
      let port = config::with_default(
        config::nested_integer(&p, "EFFECT_CONFIG_TEST_SERVER", "PORT"),
        9,
      )
      .expect("port default");
      assert_eq!(port, 9);
    });
  }

  #[test]
  fn map_provider_seq_delim() {
    let mut m = std::collections::HashMap::new();
    m.insert("TAGS".into(), "a,b, c".into());
    let opts = ProviderOptions {
      path_delim: "_",
      seq_delim: ",",
    };
    let p = MapConfigProvider::with_options(m, opts);
    let tags = config::string_list(&p, "TAGS").expect("tags");
    assert_eq!(tags, vec!["a", "b", "c"]);
  }

  #[test]
  fn figment_provider_scalar_from_toml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("c.toml");
    std::fs::write(
      &path,
      r#"
[server]
host = "0.0.0.0"
port = 3000
"#,
    )
    .expect("write");
    let fig = figment::from_toml_file(&path);
    let p = FigmentConfigProvider::new(fig);
    assert_eq!(
      config::nested_string(&p, "server", "host").unwrap(),
      "0.0.0.0"
    );
    assert_eq!(config::nested_integer(&p, "server", "port").unwrap(), 3000);
  }

  #[test]
  fn map_from_pairs() {
    let p = MapConfigProvider::from_pairs([("A_B", "x")]);
    assert_eq!(config::nested_string(&p, "A", "B").unwrap(), "x");
  }

  #[test]
  fn or_else_fallback() {
    let a = MapConfigProvider::from_map(std::collections::HashMap::new());
    let b = MapConfigProvider::from_pairs([("K", "from-b")]);
    let p = OrElseConfigProvider::new(a, b);
    assert_eq!(config::string(&p, "K").unwrap(), "from-b");
  }

  #[test]
  fn figment_provider_layer_builds_provider() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("d.toml");
    std::fs::write(&path, "k = \"v\"").expect("write");
    let env = ::id_effect::build_env([provide_figment_config_provider(figment::from_toml_file(
      &path,
    ))])
    .expect("env");
    let v: String =
      ::id_effect::run_blocking(Config::string("k").run::<String, ConfigError, _>(), env).unwrap();
    assert_eq!(v, "v");
  }

  #[test]
  fn env_provider_live() {
    use ::id_effect::{build_env, provide, run_blocking};
    let key = "EFFECT_CONFIG_TEST_LAYER_X";
    with_var(key, Some("42"), || {
      let env = build_env([provide!(EnvConfigProviderLive)]).expect("env");
      let n: i64 = run_blocking(Config::integer(key).run::<i64, ConfigError, _>(), env).unwrap();
      assert_eq!(n, 42);
    });
  }

  #[test]
  fn scoped_provider_prefix_segments_and_nested_lookup() {
    let p = MapConfigProvider::from_pairs([("A_B_C_D", "nested")]);
    let scoped = ScopedConfigProvider::new(p, "A.B");
    assert_eq!(scoped.prefix_segments(), &["A", "B"]);
    assert_eq!(config::nested_string(&scoped, "C", "D").unwrap(), "nested");
    assert_eq!(config::string(scoped.inner(), "A_B_C_D").unwrap(), "nested");
  }

  #[test]
  fn figment_bool_float_and_non_scalar_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mix.toml");
    std::fs::write(
      &path,
      r#"
flag = true
pi = 2.5
bad = [1, 2]
"#,
    )
    .expect("write");
    let fig = figment::from_toml_file(&path);
    let p = FigmentConfigProvider::new(fig);
    assert!(config::boolean(&p, "flag").unwrap());
    assert!((config::number(&p, "pi").unwrap() - 2.5).abs() < f64::EPSILON);
    assert!(config::string(&p, "bad").is_err());
  }

  // ── config_env ────────────────────────────────────────────────────────────

  #[test]
  fn config_env_runs_effect() {
    use id_effect::run_blocking;

    let p = MapConfigProvider::from_pairs([("K", "v")]);
    let env = config_env(p);
    let result: String =
      run_blocking(Config::string("K").run::<String, ConfigError, _>(), env).unwrap();
    assert_eq!(result, "v");
  }

  // ── config::optional_string ───────────────────────────────────────────────

  #[test]
  fn config_optional_string_present_and_absent() {
    let p = MapConfigProvider::from_pairs([("PRESENT", "yes")]);
    assert_eq!(
      config::optional_string(&p, "PRESENT").unwrap(),
      Some("yes".to_string())
    );
    assert_eq!(config::optional_string(&p, "ABSENT").unwrap(), None);
  }

  // ── config::number ────────────────────────────────────────────────────────

  #[test]
  fn config_number_scalar() {
    let p = MapConfigProvider::from_pairs([("PI", "3.14")]);
    let v = config::number(&p, "PI").unwrap();
    #[allow(clippy::approx_constant)]
    let expected = 3.14_f64;
    assert!((v - expected).abs() < f64::EPSILON);
  }

  // ── config::integer ───────────────────────────────────────────────────────

  #[test]
  fn config_integer_scalar() {
    let p = MapConfigProvider::from_pairs([("N", "99")]);
    assert_eq!(config::integer(&p, "N").unwrap(), 99);
  }

  // ── config::boolean ───────────────────────────────────────────────────────

  #[test]
  fn config_boolean_scalar() {
    let p = MapConfigProvider::from_pairs([("F", "true")]);
    assert!(config::boolean(&p, "F").unwrap());
  }

  // ── config::nested_optional_string ───────────────────────────────────────

  #[test]
  fn config_nested_optional_string() {
    let p = MapConfigProvider::from_pairs([("NS_KEY", "val")]);
    assert_eq!(
      config::nested_optional_string(&p, "NS", "KEY").unwrap(),
      Some("val".to_string())
    );
    assert_eq!(
      config::nested_optional_string(&p, "NS", "MISSING").unwrap(),
      None
    );
  }

  // ── config::nested_number / nested_integer / nested_boolean ──────────────

  #[test]
  fn config_nested_number() {
    let p = MapConfigProvider::from_pairs([("SRV_RATE", "1.5")]);
    let v = config::nested_number(&p, "SRV", "RATE").unwrap();
    assert!((v - 1.5).abs() < f64::EPSILON);
  }

  #[test]
  fn config_nested_integer() {
    let p = MapConfigProvider::from_pairs([("SRV_PORT", "5432")]);
    assert_eq!(config::nested_integer(&p, "SRV", "PORT").unwrap(), 5432);
  }

  #[test]
  fn config_nested_boolean() {
    let p = MapConfigProvider::from_pairs([("SRV_TLS", "true")]);
    assert!(config::nested_boolean(&p, "SRV", "TLS").unwrap());
  }

  // ── config::nested_string_list ────────────────────────────────────────────

  #[test]
  fn config_nested_string_list() {
    let p = MapConfigProvider::from_pairs([("NS_TAGS", "a,b,c")]);
    let tags = config::nested_string_list(&p, "NS", "TAGS").unwrap();
    assert_eq!(tags, vec!["a", "b", "c"]);
  }

  // ── config::string_list ───────────────────────────────────────────────────

  #[test]
  fn config_string_list_free_fn() {
    let p = MapConfigProvider::from_pairs([("HOSTS", "h1,h2")]);
    let hosts = config::string_list(&p, "HOSTS").unwrap();
    assert_eq!(hosts, vec!["h1", "h2"]);
  }

  // ── config::with_default ──────────────────────────────────────────────────

  #[test]
  fn config_with_default_missing_uses_default() {
    let p = MapConfigProvider::from_pairs::<[(&str, &str); 0], _, _>([]);
    let v = config::with_default(config::string(&p, "MISSING"), "fallback".to_string()).unwrap();
    assert_eq!(v, "fallback");
  }

  #[test]
  fn config_with_default_present_ignores_default() {
    let p = MapConfigProvider::from_pairs([("K", "real")]);
    let v = config::with_default(config::string(&p, "K"), "fallback".to_string()).unwrap();
    assert_eq!(v, "real");
  }

  #[test]
  fn config_with_default_invalid_propagates() {
    let p = MapConfigProvider::from_pairs([("N", "bad")]);
    let err = config::with_default(config::integer(&p, "N"), 0_i64).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  // ── FigmentLayer::from_shared / figment() ─────────────────────────────────

  #[test]
  fn figment_layer_from_shared_and_figment_accessor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("shared.toml");
    std::fs::write(&path, "n = 3\ns = \"hi\"").expect("write");
    let shared = Arc::new(figment::from_toml_file(&path));
    let layer = FigmentLayer::<Cfg>::from_shared(Arc::clone(&shared));
    // figment() accessor
    let _ = layer.figment();
    // build still works
    let cfg = layer.build().expect("build");
    assert_eq!(cfg.n, 3);
  }

  // ── FigmentProviderLayer::from_shared / figment() ─────────────────────────

  #[test]
  fn figment_provider_layer_from_shared_and_figment_accessor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ps.toml");
    std::fs::write(&path, "k = \"shared\"").expect("write");
    let shared = Arc::new(figment::from_toml_file(&path));
    let env = ::id_effect::build_env([provide_config_provider(
      FigmentConfigProvider::from_shared(Arc::clone(&shared)),
    )])
    .expect("env");
    let v: String =
      ::id_effect::run_blocking(Config::string("k").run::<String, ConfigError, _>(), env).unwrap();
    assert_eq!(v, "shared");
  }

  // ── EnvProviderLayer::new / options() ────────────────────────────────────

  #[test]
  fn env_provider_with_custom_options() {
    use ::id_effect::build_env;
    let opts = ProviderOptions {
      path_delim: ".",
      seq_delim: ";",
    };
    let env = build_env([provide_env_config_provider(opts)]).expect("env");
    let p = id_effect::Needs::<ConfigProviderService>::need(&env);
    assert_eq!(p.0.seq_delim(), ";");
  }
  #[test]
  fn config_env_wraps_provider() {
    use ::id_effect::Needs;
    let p = MapConfigProvider::from_pairs([("K", "v")]);
    let env = config_env(p);
    let svc = Needs::<ConfigProviderService>::need(&env);
    assert_eq!(config::string(&*svc.0, "K").unwrap(), "v");
  }

  #[test]
  fn config_optional_boolean_and_nested_helpers() {
    let p = MapConfigProvider::from_pairs([("FLAG", "true"), ("A_B", "x")]);
    assert!(config::boolean(&p, "FLAG").unwrap());
    assert_eq!(config::optional_string(&p, "MISSING").unwrap(), None);
    assert_eq!(
      config::nested_optional_string(&p, "A", "B").unwrap(),
      Some("x".into())
    );
    assert!(config::nested_boolean(&p, "A", "FLAG").is_err());
  }

  #[test]
  fn nested_string_list_and_number_helpers() {
    let opts = ProviderOptions {
      path_delim: "_",
      seq_delim: ",",
    };
    let p = MapConfigProvider::with_options(
      [("A_B".into(), "1,2".into()), ("N".into(), "3.5".into())]
        .into_iter()
        .collect(),
      opts,
    );
    assert_eq!(config::number(&p, "N").unwrap(), 3.5);
    assert_eq!(
      config::nested_string_list(&p, "A", "B").unwrap(),
      vec!["1", "2"]
    );
  }
}
