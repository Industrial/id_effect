//! HashiCorp Vault KV v2 secrets as a configuration source (feature `vault`).
//!
//! [`ConfigProvider::load_raw`] is synchronous, so a remote secret store is read **once, in a
//! layer**, into an in-memory snapshot that lookups are then served from. [`VaultSettings`]
//! describes what to read; [`VaultSettings::layer`] (or the shorthand [`VaultProvider::layer`])
//! fetches every secret path when the layer is built and installs a [`VaultProvider`] as the
//! [`ConfigProviderService`](crate::ConfigProviderService) capability.
//!
//! Each path is a KV v2 secret read with `GET <addr>/v1/<mount>/data/<path>`: its `data` object
//! is merged at the root of the configuration ([`VaultSettings::path`]) or under a key
//! ([`VaultSettings::path_at`]); later paths override earlier ones key by key. Keys are used as
//! stored, so keep them lower-case like the TOML files (`db.password`).
//!
//! With [`VaultSettings::profiles`] the snapshot is merged into a [`Profiles`] stack through
//! [`Profiles::overlay`], so the precedence is **env > Vault > `<profile>.toml` > `base.toml`**:
//! production reads the managed secret, an explicit `APP_DB__PASSWORD` still wins locally.
//!
//! With [`VaultSettings::refresh_every`] a daemon fiber re-fetches the secrets on that interval
//! and swaps the snapshot atomically; a failed refresh keeps the previous snapshot (see
//! [`VaultProvider::last_refresh_error`]). The fiber is interrupted when the layer's scope
//! closes.
//!
//! Requests go through `reqwest` with rustls and need a tokio runtime (run the program with
//! `id_effect_tokio::TokioRuntime`). Failures are typed: [`ConfigError::Vault`] with a
//! [`VaultErrorKind`] (transport, HTTP status, response shape) and [`ConfigError::Invalid`] for
//! bad settings. Neither the token nor a secret value appears in an error or a `Debug` output.
//!
//! ```rust,no_run
//! use id_effect::{Effect, HCons, HNil, need};
//! use id_effect_config::{Config, ConfigError, Profiles, Secret, VaultAuth, VaultSettings};
//!
//! struct DbPassword(Secret<String>);
//!
//! # async fn run() -> Result<(), ConfigError> {
//! let vault = VaultSettings::new("https://vault.internal:8200", VaultAuth::token_from_env()?)
//!   .mount("kv")
//!   .path_at("ledger/db", "db")
//!   .profiles(Profiles::new())
//!   .refresh_every(std::time::Duration::from_secs(300));
//!
//! let password = Config::string("db.password").secret().map(DbPassword);
//! let app = vault.layer::<ConfigError>().provide_to(password.layer());
//! let program: Effect<usize, ConfigError, HCons<DbPassword, HNil>> =
//!   need::<DbPassword>().map(|p| p.0.expose().len());
//! let runtime = id_effect_tokio::TokioRuntime::current().expect("inside tokio");
//! let _exit = runtime.run(program.provide_layer(app)).await;
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use ::figment::providers::Serialized;
use ::figment::value::{Dict, Map};
use ::figment::{Figment, Metadata};
use ::id_effect::{Effect, HNil, Layer, Never, from_async, sleep, succeed};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{ConfigError, VaultErrorKind};
#[cfg(all(feature = "toml", feature = "env"))]
use crate::profile::{Profile, Profiles};
use crate::provider::{ConfigProvider, FigmentConfigProvider};
use crate::secret::Secret;
use crate::{ConfigEnv, config_env};

/// KV v2 mount read when [`VaultSettings::mount`] is not set (the dev server's default).
pub const DEFAULT_MOUNT: &str = "secret";

/// Per-request timeout used when [`VaultSettings::timeout`] is not set.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Environment variable read by [`VaultAuth::token_from_env`] (the Vault CLI's).
pub const TOKEN_ENV_VAR: &str = "VAULT_TOKEN";

/// Header carrying the token.
pub const TOKEN_HEADER: &str = "X-Vault-Token";

/// Header carrying the namespace ([`VaultSettings::namespace`], Vault Enterprise / HCP).
pub const NAMESPACE_HEADER: &str = "X-Vault-Namespace";

/// How requests authenticate to Vault.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum VaultAuth {
  /// A token sent as [`TOKEN_HEADER`] (service, batch, or a dev server's root token).
  Token(Secret<String>),
}

impl VaultAuth {
  /// Token authentication with `token`.
  pub fn token(token: impl Into<String>) -> Self {
    Self::Token(Secret::new(token.into()))
  }

  /// Token authentication with the token in [`TOKEN_ENV_VAR`] (`VAULT_TOKEN`).
  pub fn token_from_env() -> Result<Self, ConfigError> {
    Self::token_from_var(TOKEN_ENV_VAR)
  }

  /// Token authentication with the token in the environment variable `var`. An unset or empty
  /// variable is [`ConfigError::Missing`]; a non-UTF-8 one is [`ConfigError::InvalidUtf8`].
  pub fn token_from_var(var: &str) -> Result<Self, ConfigError> {
    match std::env::var(var) {
      Ok(token) if !token.is_empty() => Ok(Self::token(token)),
      Ok(_) | Err(std::env::VarError::NotPresent) => Err(ConfigError::Missing { path: var.into() }),
      Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError::InvalidUtf8 { var: var.into() }),
    }
  }

  fn token_value(&self) -> &str {
    match self {
      Self::Token(token) => token.expose(),
    }
  }
}

/// One secret to read, and where its keys land.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SecretPath {
  path: String,
  at: Option<String>,
}

/// What to read from Vault and how: address, authentication, KV v2 mount, namespace, secret
/// paths, refresh interval. Build with [`VaultSettings::new`] and the builder methods, then
/// [`VaultSettings::layer`] or [`VaultSettings::fetch`]. See the [module documentation](self).
#[derive(Clone, Debug)]
pub struct VaultSettings {
  addr: String,
  auth: VaultAuth,
  mount: String,
  namespace: Option<String>,
  paths: Vec<SecretPath>,
  refresh_every: Option<Duration>,
  timeout: Duration,
  #[cfg(all(feature = "toml", feature = "env"))]
  profiles: Option<Profiles>,
}

impl VaultSettings {
  /// Read from the server at `addr` (`https://vault.internal:8200`) with `auth`, on mount
  /// [`DEFAULT_MOUNT`], with no namespace, no refresh and a [`DEFAULT_TIMEOUT`] per request.
  /// Add at least one secret with [`path`](Self::path) or [`path_at`](Self::path_at).
  pub fn new(addr: impl Into<String>, auth: VaultAuth) -> Self {
    Self {
      addr: addr.into(),
      auth,
      mount: DEFAULT_MOUNT.into(),
      namespace: None,
      paths: Vec::new(),
      refresh_every: None,
      timeout: DEFAULT_TIMEOUT,
      #[cfg(all(feature = "toml", feature = "env"))]
      profiles: None,
    }
  }

  /// Read from the KV v2 engine mounted at `mount` (`kv`, or a nested `team-a/kv`) instead of
  /// [`DEFAULT_MOUNT`].
  #[must_use]
  pub fn mount(mut self, mount: impl Into<String>) -> Self {
    self.mount = mount.into();
    self
  }

  /// Send every request in `namespace` ([`NAMESPACE_HEADER`]; Vault Enterprise / HCP).
  #[must_use]
  pub fn namespace(mut self, namespace: impl Into<String>) -> Self {
    self.namespace = Some(namespace.into());
    self
  }

  /// Read the secret at `path` (below the mount) and merge its keys at the root.
  #[must_use]
  pub fn path(mut self, path: impl Into<String>) -> Self {
    self.paths.push(SecretPath {
      path: path.into(),
      at: None,
    });
    self
  }

  /// Read every path of `paths`, in order, as with [`path`](Self::path).
  #[must_use]
  pub fn paths<I, S>(self, paths: I) -> Self
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    paths.into_iter().fold(self, Self::path)
  }

  /// Read the secret at `path` and merge its keys under the dotted key `at`: the field
  /// `password` of `path_at("ledger/db", "db")` is `db.password`.
  #[must_use]
  pub fn path_at(mut self, path: impl Into<String>, at: impl Into<String>) -> Self {
    self.paths.push(SecretPath {
      path: path.into(),
      at: Some(at.into()),
    });
    self
  }

  /// Re-fetch every secret on this interval while the layer is alive, swapping the snapshot
  /// atomically. Must be greater than zero.
  #[must_use]
  pub fn refresh_every(mut self, every: Duration) -> Self {
    self.refresh_every = Some(every);
    self
  }

  /// Fail a request that takes longer than `timeout` ([`VaultErrorKind::Transport`]).
  #[must_use]
  pub fn timeout(mut self, timeout: Duration) -> Self {
    self.timeout = timeout;
    self
  }

  /// Serve the secrets merged into `profiles` (env > Vault > profile file > base file) instead
  /// of on their own. The profile is resolved once, when the provider is fetched; a refresh
  /// re-reads the profile files with the secrets.
  #[cfg(all(feature = "toml", feature = "env"))]
  #[must_use]
  pub fn profiles(mut self, profiles: Profiles) -> Self {
    self.profiles = Some(profiles);
    self
  }

  /// The server address.
  #[inline]
  pub fn addr(&self) -> &str {
    &self.addr
  }

  /// The KV v2 mount.
  #[inline]
  pub fn kv_mount(&self) -> &str {
    &self.mount
  }

  /// The URL of the KV v2 read of `path`: `<addr>/v1/<mount>/data/<path>`.
  pub fn secret_url(&self, path: &str) -> String {
    format!(
      "{}/v1/{}/data/{}",
      self.addr.trim_end_matches('/'),
      self.mount.trim_matches('/'),
      path.trim_matches('/')
    )
  }

  /// An effect fetching every secret once into a new [`VaultProvider`] (no refresh fiber; call
  /// [`VaultProvider::refresh`] yourself). Invalid settings are [`ConfigError::Invalid`], read
  /// failures [`ConfigError::Vault`], both as `E::from(error)`.
  pub fn fetch<E, R>(&self) -> Effect<VaultProvider, E, R>
  where
    E: From<ConfigError> + Send + 'static,
    R: Send + Sync + 'static,
  {
    let settings = self.clone();
    from_async(move || async move { VaultProvider::connect(settings).await.map_err(E::from) })
  }

  /// A layer that fetches every secret when built and provides the [`VaultProvider`] as the
  /// [`ConfigProviderService`](crate::ConfigProviderService) capability, so
  /// [`Config::layer`](crate::Config::layer) / [`Config::run`](crate::Config::run) read it.
  /// With [`refresh_every`](Self::refresh_every) it also forks the refresh fiber, interrupted
  /// when the layer's scope closes. A failed fetch fails the build with `E::from(error)`.
  pub fn layer<E>(&self) -> Layer<ConfigEnv, E>
  where
    E: From<ConfigError> + Send + 'static,
  {
    let settings = self.clone();
    Layer::scoped(move |scope| {
      let every = settings.refresh_every;
      settings
        .fetch::<E, HNil>()
        .flat_map(move |provider: VaultProvider| {
          let env = config_env(provider.clone());
          match every {
            None => succeed(env),
            Some(every) => refresh_loop(provider, every)
              .fork_daemon()
              .widen::<E>()
              .flat_map(move |fiber| {
                scope
                  .add_finalizer(fiber.interrupt::<Never, HNil>().map(|_| ()))
                  .map(move |()| env)
              }),
          }
        })
    })
  }

  fn validate(&self) -> Result<(), ConfigError> {
    let invalid = |path: &str, value: String, reason: String| ConfigError::Invalid {
      path: path.into(),
      value,
      reason,
    };
    match reqwest::Url::parse(&self.addr) {
      Ok(url) if matches!(url.scheme(), "http" | "https") => {}
      Ok(_) => {
        return Err(invalid(
          "vault.addr",
          self.addr.clone(),
          "expected an http:// or https:// address".into(),
        ));
      }
      Err(e) => return Err(invalid("vault.addr", self.addr.clone(), e.to_string())),
    }
    if self.paths.is_empty() {
      return Err(invalid(
        "vault.paths",
        String::new(),
        "no secret paths to read".into(),
      ));
    }
    if let Some(every) = self.refresh_every
      && every.is_zero()
    {
      return Err(invalid(
        "vault.refresh_every",
        format!("{every:?}"),
        "must be greater than zero".into(),
      ));
    }
    Ok(())
  }
}

/// Sleep `every`, refresh (keeping the old snapshot on failure), repeat until interrupted.
fn refresh_loop(provider: VaultProvider, every: Duration) -> Effect<(), Never, HNil> {
  sleep::<Never, HNil>(every).flat_map(move |()| {
    let inner = Arc::clone(&provider.inner);
    from_async(move || async move {
      // A failure is recorded in `last_error`; the previous snapshot keeps serving.
      let _ = inner.reload().await;
      Ok::<(), Never>(())
    })
    .flat_map(move |()| refresh_loop(provider, every))
  })
}

/// The configuration the secrets are merged into (none, or a selected profile).
#[derive(Clone, Debug, Default)]
struct Base {
  #[cfg(all(feature = "toml", feature = "env"))]
  profile: Option<(Profiles, Profile)>,
}

impl Base {
  fn resolve(settings: &VaultSettings) -> Result<Self, ConfigError> {
    #[cfg(all(feature = "toml", feature = "env"))]
    if let Some(profiles) = &settings.profiles {
      return Ok(Self {
        profile: Some((profiles.clone(), profiles.resolve()?)),
      });
    }
    let _ = settings;
    Ok(Self::default())
  }

  fn snapshot(&self, vault: Figment) -> Result<Snapshot, ConfigError> {
    #[cfg(all(feature = "toml", feature = "env"))]
    if let Some((profiles, profile)) = &self.profile {
      let merged = profiles
        .clone()
        .overlay(vault.clone())
        .load_for(profile.clone())?;
      return Ok(Snapshot {
        view: merged.provider(),
        vault,
      });
    }
    Ok(Snapshot {
      view: FigmentConfigProvider::new(vault.clone()),
      vault,
    })
  }
}

/// One fetch: the secrets alone, and the view lookups are served from.
struct Snapshot {
  vault: Figment,
  view: FigmentConfigProvider,
}

struct Inner {
  settings: VaultSettings,
  client: Client,
  base: Base,
  state: RwLock<Arc<Snapshot>>,
  generation: AtomicU64,
  last_error: Mutex<Option<String>>,
}

impl Inner {
  /// Fetch again; swap the snapshot on success, record the error on failure.
  async fn reload(&self) -> Result<(), ConfigError> {
    let result = match fetch_all(&self.settings, &self.client).await {
      Ok(vault) => self.base.snapshot(vault),
      Err(e) => Err(e),
    };
    let mut last_error = self
      .last_error
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    match result {
      Ok(snapshot) => {
        *self.state.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(snapshot);
        self.generation.fetch_add(1, Ordering::SeqCst);
        *last_error = None;
        Ok(())
      }
      Err(e) => {
        *last_error = Some(e.to_string());
        Err(e)
      }
    }
  }
}

/// An in-memory snapshot of Vault secrets, served as a [`ConfigProvider`] (dotted lower-case
/// paths, like [`FigmentConfigProvider`]) and as a [`figment::Provider`]
/// (for [`Profiles::overlay`](crate::Profiles::overlay)). Cloning shares the snapshot.
///
/// Obtain one from [`VaultSettings::fetch`], or let [`VaultSettings::layer`] /
/// [`VaultProvider::layer`] install it. Its `Debug` output names the server and paths, never a
/// value.
#[derive(Clone)]
pub struct VaultProvider {
  inner: Arc<Inner>,
}

impl VaultProvider {
  /// Shorthand for `VaultSettings::new(addr, auth).paths(paths).layer()`: fetch the KV v2
  /// secrets at `paths` (mount [`DEFAULT_MOUNT`]) once and install them as the configuration
  /// provider. Use [`VaultSettings`] for a mount, namespace, profiles or refresh.
  pub fn layer<E, I, S>(addr: impl Into<String>, auth: VaultAuth, paths: I) -> Layer<ConfigEnv, E>
  where
    E: From<ConfigError> + Send + 'static,
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    VaultSettings::new(addr, auth).paths(paths).layer()
  }

  async fn connect(settings: VaultSettings) -> Result<Self, ConfigError> {
    settings.validate()?;
    let base = Base::resolve(&settings)?;
    let client = Client::builder()
      .timeout(settings.timeout)
      .build()
      .map_err(|e| ConfigError::Vault {
        path: String::new(),
        kind: VaultErrorKind::Transport,
        reason: e.to_string(),
      })?;
    let snapshot = base.snapshot(fetch_all(&settings, &client).await?)?;
    Ok(Self {
      inner: Arc::new(Inner {
        settings,
        client,
        base,
        state: RwLock::new(Arc::new(snapshot)),
        generation: AtomicU64::new(1),
        last_error: Mutex::new(None),
      }),
    })
  }

  /// An effect re-fetching every secret now. On success the snapshot is swapped atomically
  /// (readers see the old or the new one, never a mix); on failure the previous snapshot keeps
  /// serving and the error is returned as `E::from(error)`.
  pub fn refresh<E, R>(&self) -> Effect<(), E, R>
  where
    E: From<ConfigError> + Send + 'static,
    R: Send + Sync + 'static,
  {
    let inner = Arc::clone(&self.inner);
    from_async(move || async move { inner.reload().await.map_err(E::from) })
  }

  /// Number of successful fetches so far (1 after the initial one).
  pub fn generation(&self) -> u64 {
    self.inner.generation.load(Ordering::SeqCst)
  }

  /// The error of the latest refresh if it failed (cleared by the next successful one).
  pub fn last_refresh_error(&self) -> Option<String> {
    self
      .inner
      .last_error
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  /// The settings this provider was fetched with.
  pub fn settings(&self) -> &VaultSettings {
    &self.inner.settings
  }

  /// The secret at the dotted key `key` (`db.password`) of the current snapshot, as
  /// [`Secret<T>`]. Absent is [`ConfigError::Missing`]; a value that is not a `T` is
  /// [`ConfigError::Invalid`] with the value redacted.
  pub fn secret<T: DeserializeOwned + zeroize::Zeroize>(
    &self,
    key: &str,
  ) -> Result<Secret<T>, ConfigError> {
    let snapshot = self.snapshot();
    if !snapshot.vault.contains(key) {
      return Err(ConfigError::Missing { path: key.into() });
    }
    snapshot
      .vault
      .extract_inner::<T>(key)
      .map(Secret::new)
      .map_err(|_| ConfigError::Invalid {
        path: key.into(),
        value: "<redacted>".into(),
        reason: format!("the secret is not a {}", std::any::type_name::<T>()),
      })
  }

  /// The current snapshot of the secrets alone (without profile files or environment).
  pub fn figment(&self) -> Figment {
    self.snapshot().vault.clone()
  }

  fn snapshot(&self) -> Arc<Snapshot> {
    Arc::clone(
      &self
        .inner
        .state
        .read()
        .unwrap_or_else(PoisonError::into_inner),
    )
  }
}

impl fmt::Debug for VaultProvider {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let settings = &self.inner.settings;
    let paths: Vec<&str> = settings.paths.iter().map(|p| p.path.as_str()).collect();
    f.debug_struct("VaultProvider")
      .field("addr", &settings.addr)
      .field("mount", &settings.mount)
      .field("namespace", &settings.namespace)
      .field("paths", &paths)
      .field("generation", &self.generation())
      .finish_non_exhaustive()
  }
}

impl ConfigProvider for VaultProvider {
  fn load_raw(&self, path: &[&str]) -> Result<Option<String>, ConfigError> {
    self.snapshot().view.load_raw(path)
  }
}

impl ::figment::Provider for VaultProvider {
  fn metadata(&self) -> Metadata {
    Metadata::named("HashiCorp Vault")
  }

  fn data(&self) -> Result<Map<::figment::Profile, Dict>, ::figment::Error> {
    ::figment::Provider::data(&self.snapshot().vault)
  }
}

/// Read every configured path, in order, into one figment.
async fn fetch_all(settings: &VaultSettings, client: &Client) -> Result<Figment, ConfigError> {
  let mut figment = Figment::new();
  for secret in &settings.paths {
    let data = read_secret(settings, client, &secret.path).await?;
    figment = figment.merge(match &secret.at {
      Some(key) => Serialized::default(key, data),
      None => Serialized::defaults(data),
    });
  }
  Ok(figment)
}

/// The `data.data` object of the KV v2 read of `path`.
async fn read_secret(
  settings: &VaultSettings,
  client: &Client,
  path: &str,
) -> Result<Value, ConfigError> {
  let fail = |kind, reason: String| ConfigError::Vault {
    path: path.into(),
    kind,
    reason,
  };
  let mut request = client
    .get(settings.secret_url(path))
    .header(TOKEN_HEADER, settings.auth.token_value());
  if let Some(namespace) = &settings.namespace {
    request = request.header(NAMESPACE_HEADER, namespace);
  }
  let response = request
    .send()
    .await
    .map_err(|e| fail(VaultErrorKind::Transport, e.to_string()))?;
  let status = response.status();
  let body = response
    .bytes()
    .await
    .map_err(|e| fail(VaultErrorKind::Transport, e.to_string()))?;
  if !status.is_success() {
    let reason = vault_errors(&body)
      .unwrap_or_else(|| status.canonical_reason().unwrap_or("request failed").into());
    return Err(fail(VaultErrorKind::Status(status.as_u16()), reason));
  }
  let mut body: Value = serde_json::from_slice(&body)
    .map_err(|e| fail(VaultErrorKind::Decode, format!("invalid JSON: {e}")))?;
  match body.pointer_mut("/data/data").map(Value::take) {
    Some(data @ Value::Object(_)) => Ok(data),
    _ => Err(fail(
      VaultErrorKind::Decode,
      "no `data.data` object (is the mount a KV version 2 engine?)".into(),
    )),
  }
}

/// Vault's `{"errors": [..]}` list, joined, when the body has a non-empty one.
fn vault_errors(body: &[u8]) -> Option<String> {
  let body: Value = serde_json::from_slice(body).ok()?;
  let errors: Vec<&str> = body
    .get("errors")?
    .as_array()?
    .iter()
    .filter_map(Value::as_str)
    .collect();
  (!errors.is_empty()).then(|| errors.join("; "))
}

#[cfg(test)]
mod tests;
