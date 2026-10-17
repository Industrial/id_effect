//! Configuration aligned with [Effect.ts configuration](https://effect.website/docs/configuration):
//! one descriptor API, [`Config<T>`], plus Figment helpers for whole-document deserialization.
//!
//! ## `Config<T>` (Effect `Config.string` / `Config.withDefault` / `Config.all`)
//!
//! Compose lazy descriptors, then evaluate them one of three ways:
//!
//! - [`Config::run`] — an effect reading the [`ConfigProviderService`] capability from its
//!   environment `R` (`R: Selector<ConfigProviderService, I>`, Effect `yield* config`);
//! - [`Config::layer`] — a [`Layer`](id_effect::Layer) providing the loaded value as a
//!   capability (Effect `Layer.effect(Tag, config)`);
//! - [`Config::load`] — synchronously against a provider reference (tests, edges).
//!
//! [`Config::all`] accumulates errors: every missing or invalid key is reported (one
//! [`ConfigError::And`]), not only the first.
//!
//! ```rust
//! use id_effect::Exit;
//! use id_effect_config::{Config, ConfigError, MapConfigProvider, config_env};
//!
//! let p = MapConfigProvider::from_pairs([("HOST", "localhost"), ("PORT", "8080")]);
//!
//! // Descriptors — nothing is read yet
//! let host_cfg = Config::string("HOST");
//! let port_cfg = Config::integer("PORT").with_default(3000);
//!
//! // Evaluate with a direct provider (synchronous)
//! let (host, port) = Config::all((host_cfg.clone(), port_cfg.clone())).load(&p).unwrap();
//! assert_eq!(host, "localhost");
//! assert_eq!(port, 8080);
//!
//! // Evaluate as a Effect with the provider capability injected
//! let both = Config::all((host_cfg, port_cfg))
//!   .run::<(String, i64), ConfigError, _, _>()
//!   .provide_all(config_env(p));
//! assert!(matches!(both.block_on(), Exit::Success((h, 8080)) if h == "localhost"));
//! ```
//!
//! ## Providers are capabilities
//!
//! A [`ConfigProvider`] (env, map, Figment, fallback chains, scoped) is installed as the
//! [`ConfigProviderService`] capability: [`ConfigProvider::layer`] (Effect
//! `Layer.setConfigProvider`), [`provide_config_provider`], [`EnvConfigProviderLive::layer`], or
//! [`config_env`] for a ready environment value.
//!
//! ```rust
//! use id_effect::Exit;
//! use id_effect::{Effect, HCons, HNil, need};
//! use id_effect_config::{Config, ConfigError, ConfigProvider, MapConfigProvider};
//!
//! #[derive(Debug)]
//! struct Server {
//!   host: String,
//!   port: i64,
//! }
//!
//! let server = Config::all((Config::string("HOST"), Config::integer("PORT")))
//!   .map(|(host, port)| Server { host, port })
//!   .layer::<ConfigError>();
//! let app = MapConfigProvider::from_pairs([("HOST", "::1"), ("PORT", "80")])
//!   .layer()
//!   .provide_to(server);
//! let program: Effect<String, ConfigError, HCons<Server, HNil>> =
//!   need::<Server>().map(|s| format!("{}:{}", s.host, s.port));
//! assert!(matches!(program.provide_layer(app).block_on(), Exit::Success(a) if a == "::1:80"));
//! ```
//!
//! ## Figment + serde (whole-document extract)
//!
//! Build a [`Figment`](https://docs.rs/figment/latest/figment/struct.Figment.html) (layering TOML, JSON, env, …), then [`extract`] /
//! [`FigmentLayer`] — good for structured config files. [`FigmentConfigProvider`] exposes a
//! Figment to `Config<T>` descriptors.
//!
//! ## Profiles (`dev` / `test` / `prod`)
//!
//! With the `toml` and `env` features (default), [`Profiles`] selects a profile with
//! `--profile <name>` or `ID_EFFECT_PROFILE` and merges `config/base.toml`, then
//! `config/<profile>.toml`, then `APP_`-prefixed environment variables (env > profile > base).
//! An undeclared profile is [`ConfigError::UnknownProfile`]. See the [`profile`] module.
//!
//! [`Profiles::watch`] keeps a profile current without a restart: it polls the profile files by
//! modification time and publishes each re-parsed value through a
//! [`SubscriptionRef`](id_effect::SubscriptionRef); a change that does not parse keeps the
//! previous value and is reported. See the [`watch`] module.
//!
//! ## Secrets from HashiCorp Vault (feature `vault`)
//!
//! `vault::VaultSettings` reads KV v2 secrets over HTTP once, in a layer, into an in-memory
//! provider (optionally re-fetched every `refresh_every` and swapped atomically). Merged with
//! profiles it sits above the profile files and below the environment; failures are
//! [`ConfigError::Vault`]. See the `vault` module.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
mod config_desc;
mod error;
#[cfg(all(feature = "toml", feature = "env"))]
pub mod profile;
mod provider;
mod providers;
mod secret;
#[cfg(feature = "vault")]
pub mod vault;
#[cfg(all(feature = "toml", feature = "env"))]
pub mod watch;

pub use config_desc::{Config, ConfigAll};
pub use error::{ConfigError, VaultErrorKind};
#[cfg(all(feature = "toml", feature = "env"))]
pub use profile::{PROFILE_ENV_VAR, PROFILE_FLAG, Profile, ProfiledConfig, Profiles};
pub use provider::{
  ConfigProvider, ConfigProviderService, EnvConfigProvider, FigmentConfigProvider,
  MapConfigProvider, OrElseConfigProvider, ProviderOptions, ScopedConfigProvider,
};
pub use providers::{
  EnvConfigProviderLive, provide_config_provider, provide_env_config_provider,
  provide_figment_config_provider,
};
pub use secret::Secret;
#[cfg(feature = "vault")]
pub use vault::{VaultAuth, VaultProvider, VaultSettings};
#[cfg(all(feature = "toml", feature = "env"))]
pub use watch::ProfileWatch;

// ── Service environment helper ────────────────────────────────────────────────

use std::sync::Arc;

use ::id_effect::{HCons, HNil, hlist};

/// The minimal typed environment containing only [`ConfigProviderService`].
///
/// Use with [`config_env`] and `.provide_all(..)` to evaluate [`Config::run`] effects in tests
/// and CLI entry points; it is also the output of the provider layers and the input of
/// [`Config::layer`].
pub type ConfigEnv = HCons<ConfigProviderService, HNil>;

/// Build a minimal [`ConfigEnv`] wrapping `provider`.
///
/// ```rust
/// use id_effect::Exit;
/// use id_effect_config::{Config, MapConfigProvider, config_env, ConfigError};
///
/// let p = MapConfigProvider::from_pairs([("HOST", "localhost")]);
/// let host = Config::string("HOST")
///   .run::<String, ConfigError, _, _>()
///   .provide_all(config_env(p))
///   .block_on();
/// assert!(matches!(host, Exit::Success(h) if h == "localhost"));
/// ```
pub fn config_env<P: ConfigProvider + 'static>(provider: P) -> ConfigEnv {
  hlist![ConfigProviderService(Arc::new(provider))]
}

#[cfg(test)]
pub(crate) mod test_support {
  //! Test helper over the `Effect::block_on` edge that returns the typed outcome as a `Result`.
  use ::id_effect::{Effect, HList};

  /// Provide `env`, run on the global runtime, and return the typed outcome (panics on a defect).
  pub(crate) fn run_blocking<A, E, R>(eff: Effect<A, E, R>, env: R) -> Result<A, E>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: HList,
  {
    eff
      .provide_all(env)
      .block_on()
      .into_result()
      .map_err(|cause| cause.fail_option().expect("no typed failure in the cause"))
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
  use crate::test_support::run_blocking;
  use ::id_effect::Exit;
  use ::id_effect::Layer;
  use ::id_effect::Never;
  use serde::Deserialize;
  use std::io::Write;
  use temp_env::with_var;

  /// Build a provider layer into its environment value.
  fn build_layer(layer: Layer<ConfigEnv, Never>) -> ConfigEnv {
    match layer.build().block_on() {
      Exit::Success(env) => env,
      Exit::Failure(_) => unreachable!("provider layers never fail"),
    }
  }

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

  fn write_file(dir: &tempfile::TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, body).expect("write");
    path
  }

  #[test]
  fn try_extract_matches_extract() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "a.toml", "n = 3\ns = \"t\"");
    let fig = figment::from_toml_file(&path);
    let cfg: Cfg = try_extract(&fig).expect("try_extract");
    assert_eq!(cfg.n, 3);
    assert!(matches!(
      try_extract::<Cfg>(&::figment::Figment::new()),
      Err(ConfigError::Figment(_))
    ));
  }

  #[test]
  fn merge_toml_overrides_earlier_layers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = write_file(&dir, "base.toml", "n = 1\ns = \"base\"");
    let local = write_file(&dir, "local.toml", "s = \"local\"");
    let fig = figment::merge_toml(figment::from_toml_file(&base), &local);
    let cfg: Cfg = extract(&fig).expect("extract");
    assert_eq!((cfg.n, cfg.s.as_str()), (1, "local"));
  }

  #[cfg(feature = "json")]
  #[test]
  fn merge_json_overrides_toml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = write_file(&dir, "base.toml", "n = 1\ns = \"base\"");
    let json = write_file(&dir, "o.json", r#"{"n": 2}"#);
    let cfg: Cfg = extract(&figment::merge_json(figment::from_toml_file(&base), &json)).unwrap();
    assert_eq!((cfg.n, cfg.s.as_str()), (2, "base"));
  }

  #[cfg(feature = "yaml")]
  #[test]
  fn merge_yaml_overrides_toml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = write_file(&dir, "base.toml", "n = 1\ns = \"base\"");
    let yaml = write_file(&dir, "o.yaml", "s: yaml\n");
    let cfg: Cfg = extract(&figment::merge_yaml(figment::from_toml_file(&base), &yaml)).unwrap();
    assert_eq!((cfg.n, cfg.s.as_str()), (1, "yaml"));
  }

  #[cfg(feature = "env")]
  #[test]
  fn env_figments_read_process_environment() {
    temp_env::with_vars(
      [
        ("IDCFGT_N", Some("5")),
        ("IDCFGT_S", Some("env")),
        ("IDCFGT_RAW_MARKER", Some("raw")),
      ],
      || {
        let cfg: Cfg = extract(&figment::from_env_prefixed("IDCFGT_")).expect("prefixed");
        assert_eq!((cfg.n, cfg.s.as_str()), (5, "env"));

        #[derive(Deserialize)]
        struct Raw {
          idcfgt_raw_marker: String,
        }
        let raw: Raw = extract(&figment::from_env_raw()).expect("raw");
        assert_eq!(raw.idcfgt_raw_marker, "raw");
      },
    );
  }

  #[cfg(feature = "env")]
  #[test]
  fn layered_toml_env_applies_env_last() {
    let dir = tempfile::tempdir().expect("tempdir");
    let default = write_file(&dir, "default.toml", "n = 1\ns = \"default\"");
    let local = write_file(&dir, "local.toml", "n = 2");
    with_var("IDCFGL_S", Some("from-env"), || {
      let cfg: Cfg = extract(&figment::layered_toml_env(&default, &local, "IDCFGL_")).unwrap();
      assert_eq!((cfg.n, cfg.s.as_str()), (2, "from-env"));
    });
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
      let host = Config::string("HOST")
        .nested("EFFECT_CONFIG_TEST_SERVER")
        .load(&p)
        .expect("host");
      assert_eq!(host, "localhost");
      let port = Config::integer("PORT")
        .nested("EFFECT_CONFIG_TEST_SERVER")
        .with_default(9)
        .load(&p)
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
    let tags = Config::string_list("TAGS").load(&p).expect("tags");
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
    assert_eq!(Config::string("server.host").load(&p).unwrap(), "0.0.0.0");
    assert_eq!(
      Config::integer("port").nested("server").load(&p).unwrap(),
      3000
    );
  }

  #[test]
  fn map_from_pairs() {
    let p = MapConfigProvider::from_pairs([("A_B", "x")]);
    assert_eq!(Config::string("B").nested("A").load(&p).unwrap(), "x");
  }

  #[test]
  fn or_else_fallback() {
    let a = MapConfigProvider::from_map(std::collections::HashMap::new());
    let b = MapConfigProvider::from_pairs([("K", "from-b")]);
    let p = OrElseConfigProvider::new(a, b);
    assert_eq!(Config::string("K").load(&p).unwrap(), "from-b");
  }

  #[test]
  fn figment_provider_layer_builds_provider() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("d.toml");
    std::fs::write(&path, "k = \"v\"").expect("write");
    let env = build_layer(provide_figment_config_provider(figment::from_toml_file(
      &path,
    )));
    let v: String =
      run_blocking(Config::string("k").run::<String, ConfigError, _, _>(), env).unwrap();
    assert_eq!(v, "v");
  }

  #[test]
  fn env_provider_live() {
    let key = "EFFECT_CONFIG_TEST_LAYER_X";
    with_var(key, Some("42"), || {
      let env = build_layer(EnvConfigProviderLive::layer());
      let n: i64 = run_blocking(Config::integer(key).run::<i64, ConfigError, _, _>(), env).unwrap();
      assert_eq!(n, 42);
    });
  }

  #[test]
  fn scoped_provider_prefix_segments_and_nested_lookup() {
    let p = MapConfigProvider::from_pairs([("A_B_C_D", "nested")]);
    let scoped = ScopedConfigProvider::new(p, "A.B");
    assert_eq!(scoped.prefix_segments(), &["A", "B"]);
    assert_eq!(Config::string("C.D").load(&scoped).unwrap(), "nested");
    assert_eq!(
      Config::string("A_B_C_D").load(scoped.inner()).unwrap(),
      "nested"
    );
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
    assert!(Config::boolean("flag").load(&p).unwrap());
    assert!((Config::number("pi").load(&p).unwrap() - 2.5).abs() < f64::EPSILON);
    assert!(Config::string("bad").load(&p).is_err());
  }

  // ── config_env ────────────────────────────────────────────────────────────

  #[test]
  fn config_env_runs_effect() {
    let p = MapConfigProvider::from_pairs([("K", "v")]);
    let env = config_env(p);
    let result: String =
      run_blocking(Config::string("K").run::<String, ConfigError, _, _>(), env).unwrap();
    assert_eq!(result, "v");
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
    let env = build_layer(provide_config_provider(FigmentConfigProvider::from_shared(
      Arc::clone(&shared),
    )));
    let v: String =
      run_blocking(Config::string("k").run::<String, ConfigError, _, _>(), env).unwrap();
    assert_eq!(v, "shared");
  }

  // ── EnvProviderLayer::new / options() ────────────────────────────────────

  #[test]
  fn env_provider_with_custom_options() {
    let opts = ProviderOptions {
      path_delim: ".",
      seq_delim: ";",
    };
    let env = build_layer(provide_env_config_provider(opts));
    assert_eq!(env.0.0.seq_delim(), ";");
  }
  #[test]
  fn config_env_wraps_provider() {
    let p = MapConfigProvider::from_pairs([("K", "v")]);
    let env = config_env(p);
    let svc: &ConfigProviderService = &env.0;
    assert_eq!(Config::string("K").load(&*svc.0).unwrap(), "v");
  }
}
