//! Per-environment configuration profiles (`dev` / `test` / `prod`) as layered TOML overlays.
//!
//! A [`Profiles`] description selects one profile and merges, lowest precedence first:
//!
//! | Rank | Source                              | Example                    |
//! |------|-------------------------------------|----------------------------|
//! | 1    | `<dir>/base.toml`                   | `config/base.toml`         |
//! | 2    | `<dir>/<profile>.toml`              | `config/prod.toml`         |
//! | 3    | overlays ([`Profiles::overlay`])    | a secret-manager snapshot  |
//! | 4    | prefixed environment variables      | `APP_DB__PASSWORD`         |
//!
//! A later source overrides an earlier one key by key (**env > overlays > profile > base**).
//! Environment keys drop the prefix, are lower-cased, and use `__` for nesting: with the default
//! prefix `APP_`, `APP_DB__PASSWORD` sets `db.password`. Missing TOML files are skipped; a file
//! that does not parse fails [`Profiles::load`].
//!
//! [`Profiles::load`] reads the files once. [`Profiles::watch`] also polls them by modification
//! time and publishes every re-parsed value to subscribers; a change that does not parse keeps
//! the previous value (see the [`watch`](crate::watch) module).
//!
//! The profile itself is selected by, highest priority first: the `--profile <name>` (or
//! `--profile=<name>`) flag, the [`PROFILE_ENV_VAR`] (`ID_EFFECT_PROFILE`) variable, then the
//! default profile (`dev`). A name outside the declared profiles is a typed
//! [`ConfigError::UnknownProfile`], never a silently empty overlay.
//!
//! ```rust
//! use id_effect_config::{Config, ConfigError, Profiles};
//!
//! let dir = tempfile::tempdir().unwrap();
//! std::fs::write(dir.path().join("base.toml"), "port = 3000\nhost = \"localhost\"").unwrap();
//! std::fs::write(dir.path().join("prod.toml"), "host = \"0.0.0.0\"").unwrap();
//!
//! let profiles = Profiles::new().dir(dir.path()).env_prefix("DOC_PROFILE_APP_");
//! let prod = profiles.select(["--profile", "prod"], None)?;
//! let cfg = profiles.load_for(prod)?;
//! let provider = cfg.provider();
//! assert_eq!(Config::string("host").load(&provider)?, "0.0.0.0");
//! assert_eq!(Config::integer("port").load(&provider)?, 3000);
//!
//! let unknown = profiles.select(Vec::<String>::new(), Some("staging")).unwrap_err();
//! assert!(matches!(unknown, ConfigError::UnknownProfile { .. }));
//! # Ok::<(), ConfigError>(())
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use ::figment::Figment;
use ::figment::providers::{Env, Format, Toml};
use ::id_effect::{Layer, fail, succeed};
use serde::de::DeserializeOwned;

use crate::error::ConfigError;
use crate::provider::FigmentConfigProvider;
use crate::{ConfigEnv, config_env};

/// Environment variable selecting the active profile (`ID_EFFECT_PROFILE=prod`).
pub const PROFILE_ENV_VAR: &str = "ID_EFFECT_PROFILE";

/// Command-line flag selecting the active profile (`--profile prod` or `--profile=prod`).
pub const PROFILE_FLAG: &str = "--profile";

/// Profile used when neither the flag nor the environment variable selects one.
pub const DEFAULT_PROFILE: &str = "dev";

/// Directory holding `base.toml` and the `<profile>.toml` overlays, relative to the working
/// directory.
pub const DEFAULT_CONFIG_DIR: &str = "config";

/// Prefix of the environment variables merged last (`APP_DB__PASSWORD` sets `db.password`).
pub const DEFAULT_ENV_PREFIX: &str = "APP_";

/// File stem of the overlay shared by every profile (`<dir>/base.toml`).
pub const BASE_PROFILE_FILE: &str = "base";

/// A declared profile name, obtained from [`Profiles::profile`], [`Profiles::select`] or
/// [`Profiles::resolve`] (so it is always one of [`Profiles::known_profiles`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Profile(String);

impl Profile {
  /// The profile name (`"prod"`).
  #[inline]
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl AsRef<str> for Profile {
  fn as_ref(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for Profile {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// Description of an application's profiles: which exist, how one is selected and where its
/// files live. Build with [`Profiles::new`] and the builder methods, then [`Profiles::load`] or
/// [`Profiles::layer`]. See the [module documentation](self) for the precedence rules.
#[derive(Clone, Debug)]
pub struct Profiles {
  dir: PathBuf,
  known: Vec<String>,
  default: String,
  env_var: String,
  env_prefix: String,
  overlays: Vec<Figment>,
}

impl Default for Profiles {
  fn default() -> Self {
    Self::new()
  }
}

impl Profiles {
  /// Profiles `dev`, `test` and `prod` under [`DEFAULT_CONFIG_DIR`], selected by
  /// [`PROFILE_FLAG`] / [`PROFILE_ENV_VAR`] (default [`DEFAULT_PROFILE`]), environment prefix
  /// [`DEFAULT_ENV_PREFIX`].
  pub fn new() -> Self {
    Self {
      dir: PathBuf::from(DEFAULT_CONFIG_DIR),
      known: vec!["dev".into(), "test".into(), "prod".into()],
      default: DEFAULT_PROFILE.into(),
      env_var: PROFILE_ENV_VAR.into(),
      env_prefix: DEFAULT_ENV_PREFIX.into(),
      overlays: Vec::new(),
    }
  }

  /// Read `base.toml` and `<profile>.toml` from `dir` instead of [`DEFAULT_CONFIG_DIR`].
  #[must_use]
  pub fn dir(mut self, dir: impl Into<PathBuf>) -> Self {
    self.dir = dir.into();
    self
  }

  /// Replace the declared profiles; any other name is [`ConfigError::UnknownProfile`].
  #[must_use]
  pub fn known<I, S>(mut self, names: I) -> Self
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    self.known = names.into_iter().map(Into::into).collect();
    self
  }

  /// Profile used when neither the flag nor the environment variable selects one. It is
  /// checked against the declared profiles at selection time.
  #[must_use]
  pub fn default_profile(mut self, name: impl Into<String>) -> Self {
    self.default = name.into();
    self
  }

  /// Read the profile from `name` instead of [`PROFILE_ENV_VAR`] (e.g. `APP_PROFILE`).
  #[must_use]
  pub fn env_var(mut self, name: impl Into<String>) -> Self {
    self.env_var = name.into();
    self
  }

  /// Merge environment variables starting with `prefix` instead of [`DEFAULT_ENV_PREFIX`].
  #[must_use]
  pub fn env_prefix(mut self, prefix: impl Into<String>) -> Self {
    self.env_prefix = prefix.into();
    self
  }

  /// Merge `provider` above the profile files and below the environment.
  ///
  /// This is the seam for secret sources: an in-memory snapshot fetched from a secret manager
  /// overrides what the TOML files say, while an explicit environment variable still wins for
  /// local debugging. Overlays merge in the order they are added.
  #[must_use]
  pub fn overlay(mut self, provider: impl ::figment::Provider) -> Self {
    self.overlays.push(Figment::from(provider));
    self
  }

  /// The declared profile names, in declaration order.
  #[inline]
  pub fn known_profiles(&self) -> &[String] {
    &self.known
  }

  /// Directory holding the profile files.
  #[inline]
  pub fn config_dir(&self) -> &Path {
    &self.dir
  }

  /// `name` as a [`Profile`] when it is declared, else [`ConfigError::UnknownProfile`].
  pub fn profile(&self, name: &str) -> Result<Profile, ConfigError> {
    if self.known.iter().any(|k| k == name) {
      Ok(Profile(name.to_owned()))
    } else {
      Err(ConfigError::UnknownProfile {
        profile: name.to_owned(),
        known: self.known.clone(),
      })
    }
  }

  /// Select the profile from explicit inputs: the last [`PROFILE_FLAG`] in `args` wins, then a
  /// non-empty `env_value`, then the default profile.
  ///
  /// A trailing `--profile` without a value is [`ConfigError::Invalid`]; an undeclared name is
  /// [`ConfigError::UnknownProfile`].
  pub fn select<I, S>(&self, args: I, env_value: Option<&str>) -> Result<Profile, ConfigError>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
  {
    let flag = profile_flag(args)?;
    let name = flag
      .as_deref()
      .or(env_value.filter(|v| !v.is_empty()))
      .unwrap_or(&self.default);
    self.profile(name)
  }

  /// Select the profile from the process arguments and the profile environment variable
  /// (see [`Profiles::select`]). A non-UTF-8 variable is [`ConfigError::InvalidUtf8`].
  pub fn resolve(&self) -> Result<Profile, ConfigError> {
    let env_value = match std::env::var(&self.env_var) {
      Ok(v) => Some(v),
      Err(std::env::VarError::NotPresent) => None,
      Err(std::env::VarError::NotUnicode(_)) => {
        return Err(ConfigError::InvalidUtf8 {
          var: self.env_var.clone(),
        });
      }
    };
    let args = std::env::args_os()
      .skip(1)
      .map(|a| a.to_string_lossy().into_owned());
    self.select(args, env_value.as_deref())
  }

  /// Path of the overlay shared by every profile (`<dir>/base.toml`).
  pub fn base_path(&self) -> PathBuf {
    self.dir.join(format!("{BASE_PROFILE_FILE}.toml"))
  }

  /// Path of `profile`'s overlay (`<dir>/<profile>.toml`).
  pub fn profile_path(&self, profile: &Profile) -> PathBuf {
    self.dir.join(format!("{profile}.toml"))
  }

  /// The merged (lazy) [`Figment`] for `profile`: base, profile, overlays, environment.
  pub fn figment_for(&self, profile: &Profile) -> Figment {
    let files = Figment::new()
      .merge(Toml::file(self.base_path()))
      .merge(Toml::file(self.profile_path(profile)));
    self
      .overlays
      .iter()
      .fold(files, |fig, overlay| fig.merge(overlay.clone()))
      .merge(Env::prefixed(&self.env_prefix).split("__"))
  }

  /// Merge and parse every source for `profile`; a file that does not parse is
  /// [`ConfigError::Figment`].
  pub fn load_for(&self, profile: Profile) -> Result<ProfiledConfig, ConfigError> {
    let figment = self.figment_for(&profile);
    ::figment::Provider::data(&figment)?;
    Ok(ProfiledConfig { profile, figment })
  }

  /// [`Profiles::resolve`] then [`Profiles::load_for`].
  pub fn load(&self) -> Result<ProfiledConfig, ConfigError> {
    self.load_for(self.resolve()?)
  }

  /// A layer that resolves and loads the profile when built and provides it as the
  /// [`ConfigProviderService`](crate::ConfigProviderService) capability, so
  /// [`Config::layer`](crate::Config::layer) / [`Config::run`](crate::Config::run) read the
  /// merged sources. Selection and parse failures fail the build with `E::from(error)`.
  pub fn layer<E>(&self) -> Layer<ConfigEnv, E>
  where
    E: From<ConfigError> + Send + 'static,
  {
    let profiles = self.clone();
    Layer::effect(move || match profiles.load() {
      Ok(cfg) => succeed(config_env(cfg.provider())),
      Err(e) => fail(E::from(e)),
    })
  }
}

/// The value of the last [`PROFILE_FLAG`] in `args`, if any.
fn profile_flag<I, S>(args: I) -> Result<Option<String>, ConfigError>
where
  I: IntoIterator<Item = S>,
  S: AsRef<str>,
{
  let mut found = None;
  let mut args = args.into_iter();
  while let Some(arg) = args.next() {
    let arg = arg.as_ref();
    if arg == PROFILE_FLAG {
      match args.next() {
        Some(value) => found = Some(value.as_ref().to_owned()),
        None => {
          return Err(ConfigError::Invalid {
            path: PROFILE_FLAG.into(),
            value: String::new(),
            reason: "expected a profile name after the flag".into(),
          });
        }
      }
    } else if let Some(value) = arg
      .strip_prefix(PROFILE_FLAG)
      .and_then(|rest| rest.strip_prefix('='))
    {
      found = Some(value.to_owned());
    }
  }
  Ok(found)
}

/// The merged configuration of one selected [`Profile`] (from [`Profiles::load`]).
///
/// `Debug` names the profile and the merged sources but never prints values: the merged figment
/// can hold secrets (for example from the `vault` overlay).
#[derive(Clone)]
pub struct ProfiledConfig {
  profile: Profile,
  figment: Figment,
}

impl std::fmt::Debug for ProfiledConfig {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ProfiledConfig")
      .field("profile", &self.profile)
      .finish_non_exhaustive()
  }
}

impl ProfiledConfig {
  /// The selected profile.
  #[inline]
  pub fn profile(&self) -> &Profile {
    &self.profile
  }

  /// The merged [`Figment`].
  #[inline]
  pub fn figment(&self) -> &Figment {
    &self.figment
  }

  /// Take the merged [`Figment`] (for [`FigmentLayer`](crate::FigmentLayer) and friends).
  #[inline]
  pub fn into_figment(self) -> Figment {
    self.figment
  }

  /// Deserialize the whole merged document as `T` (see [`extract`](crate::extract)).
  pub fn extract<T: DeserializeOwned>(&self) -> Result<T, ConfigError> {
    crate::extract(&self.figment)
  }

  /// A [`FigmentConfigProvider`] over the merged sources, for [`Config`](crate::Config)
  /// descriptors (dotted lower-case paths: `Config::string("db.host")`).
  pub fn provider(&self) -> FigmentConfigProvider {
    FigmentConfigProvider::new(self.figment.clone())
  }

  /// A layer providing [`ProfiledConfig::provider`] as the configuration capability.
  pub fn layer<E: Send + 'static>(self) -> Layer<ConfigEnv, E> {
    crate::provide_figment_config_provider(self.figment)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Config;
  use ::id_effect::{Effect, Exit, HCons, HNil, need};
  use serde::Deserialize;

  fn write(dir: &tempfile::TempDir, name: &str, body: &str) {
    std::fs::write(dir.path().join(name), body).expect("write");
  }

  fn no_args() -> Vec<String> {
    Vec::new()
  }

  #[test]
  fn debug_names_the_profile_but_never_prints_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "base.toml", "[db]\npassword = \"s3cr3t\"\n");
    let profiles = Profiles::default().dir(dir.path());
    let loaded = profiles
      .load_for(profiles.profile("prod").unwrap())
      .expect("load");
    let shown = format!("{loaded:?}");
    assert!(shown.contains("prod"), "{shown}");
    assert!(!shown.contains("s3cr3t"), "{shown}");
  }

  #[test]
  fn defaults_declare_dev_test_prod() {
    let p = Profiles::default();
    assert_eq!(p.known_profiles(), ["dev", "test", "prod"]);
    assert_eq!(p.config_dir(), Path::new(DEFAULT_CONFIG_DIR));
    assert_eq!(p.base_path(), Path::new("config/base.toml"));
    let prod = p.profile("prod").unwrap();
    assert_eq!(p.profile_path(&prod), Path::new("config/prod.toml"));
    assert_eq!(prod.as_str(), "prod");
    assert_eq!(prod.as_ref(), "prod");
    assert_eq!(prod.to_string(), "prod");
  }

  #[test]
  fn select_prefers_flag_then_env_then_default() {
    let p = Profiles::new();
    let pick = |args: &[&str], env: Option<&str>| p.select(args, env).unwrap().to_string();
    assert_eq!(pick(&[], None), "dev");
    assert_eq!(pick(&[], Some("")), "dev");
    assert_eq!(pick(&[], Some("test")), "test");
    assert_eq!(pick(&["--profile", "prod"], Some("test")), "prod");
    assert_eq!(pick(&["--profile=prod"], Some("test")), "prod");
    assert_eq!(pick(&["--profile", "test", "--profile=prod"], None), "prod");
    assert_eq!(pick(&["--profiles=prod", "-v"], None), "dev");
  }

  #[test]
  fn select_rejects_unknown_and_dangling_flag() {
    let p = Profiles::new().known(["dev", "prod"]);
    match p.select(no_args(), Some("staging")) {
      Err(ConfigError::UnknownProfile { profile, known }) => {
        assert_eq!(profile, "staging");
        assert_eq!(known, ["dev", "prod"]);
      }
      other => panic!("expected UnknownProfile, got {other:?}"),
    }
    assert!(matches!(
      p.select(["--profile"], None),
      Err(ConfigError::Invalid { path, .. }) if path == PROFILE_FLAG
    ));
    let bad_default = Profiles::new().default_profile("local");
    assert!(matches!(
      bad_default.select(no_args(), None),
      Err(ConfigError::UnknownProfile { profile, .. }) if profile == "local"
    ));
  }

  #[test]
  fn resolve_reads_the_configured_env_var() {
    let p = Profiles::new().env_var("IDCFG_PROFILE_RESOLVE");
    temp_env::with_var("IDCFG_PROFILE_RESOLVE", Some("test"), || {
      assert_eq!(p.resolve().unwrap().as_str(), "test");
    });
    temp_env::with_var("IDCFG_PROFILE_RESOLVE", None::<&str>, || {
      assert_eq!(p.resolve().unwrap().as_str(), "dev");
    });
  }

  #[cfg(unix)]
  #[test]
  fn resolve_rejects_non_utf8_env_var() {
    use std::os::unix::ffi::OsStrExt;
    let p = Profiles::new().env_var("IDCFG_PROFILE_UTF8");
    let bad = std::ffi::OsStr::from_bytes(&[0x66, 0xff]);
    temp_env::with_var("IDCFG_PROFILE_UTF8", Some(bad), || {
      assert!(matches!(
        p.resolve(),
        Err(ConfigError::InvalidUtf8 { var }) if var == "IDCFG_PROFILE_UTF8"
      ));
    });
  }

  #[derive(Debug, Deserialize, PartialEq)]
  struct Db {
    host: String,
    password: String,
  }

  #[derive(Debug, Deserialize, PartialEq)]
  struct App {
    name: String,
    db: Db,
  }

  #[test]
  fn load_merges_base_profile_overlay_env() {
    let dir = tempfile::tempdir().unwrap();
    write(
      &dir,
      "base.toml",
      "name = \"base\"\n[db]\nhost = \"localhost\"\npassword = \"base-pw\"",
    );
    write(&dir, "prod.toml", "[db]\nhost = \"db.prod\"");
    let secrets = ::figment::providers::Serialized::default("db.password", "vault-pw");
    let p = Profiles::new()
      .dir(dir.path())
      .env_var("IDCFG_PROFILE_LOAD")
      .env_prefix("IDCFG_LOAD_")
      .overlay(secrets);
    temp_env::with_vars(
      [
        ("IDCFG_PROFILE_LOAD", Some("prod")),
        ("IDCFG_LOAD_NAME", Some("from-env")),
      ],
      || {
        let cfg = p.load().unwrap();
        assert_eq!(cfg.profile().as_str(), "prod");
        let app: App = cfg.extract().unwrap();
        assert_eq!(
          app,
          App {
            name: "from-env".into(),
            db: Db {
              host: "db.prod".into(),
              password: "vault-pw".into(),
            },
          }
        );
        assert!(cfg.figment().contains("db.host"));
        let fig = cfg.into_figment();
        assert_eq!(fig.extract_inner::<String>("db.host").unwrap(), "db.prod");
      },
    );
  }

  #[test]
  fn load_for_reports_unparseable_file() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir, "base.toml", "this is = = not toml");
    let p = Profiles::new().dir(dir.path());
    let dev = p.profile("dev").unwrap();
    assert!(matches!(p.load_for(dev), Err(ConfigError::Figment(_))));
  }

  #[test]
  fn load_fails_on_unknown_selected_profile() {
    let p = Profiles::new().env_var("IDCFG_PROFILE_UNKNOWN");
    temp_env::with_var("IDCFG_PROFILE_UNKNOWN", Some("staging"), || {
      assert!(matches!(p.load(), Err(ConfigError::UnknownProfile { .. })));
    });
  }

  struct Port(i64);

  #[test]
  fn layers_feed_config_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir, "base.toml", "port = 3000");
    write(&dir, "test.toml", "port = 4000");
    let p = Profiles::new()
      .dir(dir.path())
      .env_var("IDCFG_PROFILE_LAYER")
      .env_prefix("IDCFG_LAYER_");

    let port_of = |layer: Layer<ConfigEnv, ConfigError>| {
      let program: Effect<i64, ConfigError, HCons<Port, HNil>> = need::<Port>().map(|p| p.0);
      let port = Config::integer("port").map(Port).layer::<ConfigError>();
      program.provide_layer(layer.provide_to(port)).block_on()
    };

    temp_env::with_var("IDCFG_PROFILE_LAYER", Some("test"), || {
      assert!(matches!(port_of(p.layer()), Exit::Success(4000)));
    });
    let dev = p.load_for(p.profile("dev").unwrap()).unwrap();
    assert!(matches!(port_of(dev.layer()), Exit::Success(3000)));
    temp_env::with_var("IDCFG_PROFILE_LAYER", Some("staging"), || {
      assert!(matches!(port_of(p.layer()), Exit::Failure(_)));
    });
  }
}
