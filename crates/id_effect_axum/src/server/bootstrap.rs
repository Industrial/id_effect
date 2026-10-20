//! Config bootstrap via [`id_effect_config`]: [`HostConfig`] as a `Config<T>` descriptor and as a
//! [`Layer`].

use std::path::PathBuf;
use std::time::Duration;

use id_effect::observability::posture::record_dangerous;
use id_effect::{Effect, HCons, HNil, Layer, Selector};
use id_effect_config::{
  Config, ConfigEnv, ConfigError, ConfigProvider, ConfigProviderService, EnvConfigProviderLive,
  config_env,
};

use crate::middleware::LoadShed;

/// The posture warning of a host that enables HTTP/3 without TLS ([`HostConfig::posture_warnings`]).
pub const HTTP3_WITHOUT_TLS_WARNING: &str = "HTTP/3 is enabled but the host has no TLS: HTTP/3 \
  needs a certificate and browsers ignore Alt-Svc over plain HTTP, so no Alt-Svc is sent; set \
  HostConfig::tls (TLS_CERT_FILE and TLS_KEY_FILE) or turn HTTP/3 off (HTTP3=false)";

/// TLS on the host's TCP listener ([`HostConfig::tls`]): PEM files (re-read when they change)
/// or, with feature `tls`, ready settings.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostTls {
  /// A PEM certificate chain (leaf first) and private key; with `reload_every`, the files are
  /// polled and a rotated certificate is used for new connections.
  Files {
    /// The PEM chain.
    cert_file: PathBuf,
    /// The PEM private key (PKCS#8, PKCS#1 or SEC1).
    key_file: PathBuf,
    /// How often the files are checked for a new certificate (`None`: never).
    reload_every: Option<Duration>,
  },
  /// Ready settings (an [`Identity`](id_effect_platform::tls::Identity) of your own, a
  /// resolver that answers ACME challenges, ...). HTTP/3 is served with the settings'
  /// [`identity`](crate::server::tls::ServerTls::identity) (set by `ServerTls::from_identity`
  /// and `ServerTls::from_pem_files`, or `ServerTlsBuilder::identity` for a resolver); settings
  /// without one serve TCP only, and the host logs why.
  #[cfg(feature = "tls")]
  Provided(crate::server::tls::ServerTls),
}

impl HostTls {
  /// The default polling interval of [`Self::files`]: 60 s.
  pub const DEFAULT_RELOAD_EVERY: Duration = Duration::from_secs(60);

  /// The chain and key files, polled every [`Self::DEFAULT_RELOAD_EVERY`].
  pub fn files(cert_file: impl Into<PathBuf>, key_file: impl Into<PathBuf>) -> Self {
    Self::Files {
      cert_file: cert_file.into(),
      key_file: key_file.into(),
      reload_every: Some(Self::DEFAULT_RELOAD_EVERY),
    }
  }

  /// Ready settings.
  #[cfg(feature = "tls")]
  pub fn provided(tls: crate::server::tls::ServerTls) -> Self {
    Self::Provided(tls)
  }

  /// The listener's TLS settings: the files are read now (and polled from the current Tokio
  /// runtime when `reload_every` is set).
  ///
  /// # Errors
  ///
  /// The [`CertError`](id_effect_platform::tls::CertError) of
  /// [`ServerTls::from_pem_files`](crate::server::tls::ServerTls::from_pem_files).
  #[cfg(feature = "tls")]
  pub fn server_tls(
    &self,
  ) -> Result<crate::server::tls::ServerTls, id_effect_platform::tls::CertError> {
    match self {
      Self::Files {
        cert_file,
        key_file,
        reload_every,
      } => crate::server::tls::ServerTls::from_pem_files(
        cert_file.clone(),
        key_file.clone(),
        *reload_every,
      ),
      Self::Provided(tls) => Ok(tls.clone()),
    }
  }
}

/// Host bind, shutdown, load-shedding, TLS and HTTP/3 settings loaded from the environment
/// (`HOST`, `PORT`, `SHUTDOWN_TIMEOUT_SECS`, `MAX_IN_FLIGHT`, `TLS_CERT_FILE`, `TLS_KEY_FILE`,
/// `TLS_RELOAD_SECS`, `HTTP3`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostConfig {
  /// Bind address (e.g. `0.0.0.0`).
  pub bind_host: String,
  /// Listen port.
  pub bind_port: u16,
  /// Graceful shutdown drain timeout.
  pub shutdown_timeout: Duration,
  /// The in-flight request cap the server installs ([`LoadShedLayer`](crate::middleware::LoadShedLayer));
  /// on by default, off only through [`Self::dangerous_no_load_shedding`].
  pub load_shed: LoadShed,
  /// TLS on the TCP listener (feature `tls`; TLS 1.3 and 1.2, ALPN `h2` and `http/1.1`).
  /// `None`, the default, serves plain HTTP; set without the feature, the host refuses to
  /// start rather than serve plain HTTP.
  pub tls: Option<HostTls>,
  /// Whether HTTP/3 is served next to TCP (feature `http3`; default `true`). HTTP/3 needs
  /// [`Self::tls`]: without it the [posture warnings](Self::posture_warnings) say so and no
  /// `Alt-Svc` is sent. `false` turns HTTP/3 and `Alt-Svc` off.
  pub http3: bool,
}

impl Default for HostConfig {
  /// `0.0.0.0:8080`, a 30 s shutdown timeout, the default [`LoadShed`], plain HTTP and HTTP/3
  /// on (what [`Self::config`] loads from an empty environment).
  fn default() -> Self {
    Self {
      bind_host: "0.0.0.0".to_owned(),
      bind_port: 8080,
      shutdown_timeout: Duration::from_secs(30),
      load_shed: LoadShed::default(),
      tls: None,
      http3: true,
    }
  }
}

impl HostConfig {
  /// The raw descriptor: `HOST` (default `0.0.0.0`), `PORT` (default `8080`),
  /// `SHUTDOWN_TIMEOUT_SECS` (default `30`) and `MAX_IN_FLIGHT` (default: none, i.e.
  /// [`LoadShed::default_max_in_flight`]), loaded together so every failure is reported.
  #[inline]
  pub fn descriptor() -> Config<(String, i64, i64, Option<i64>)> {
    Config::all((
      Config::string("HOST").with_default("0.0.0.0".to_string()),
      Config::integer("PORT").with_default(8080),
      Config::integer("SHUTDOWN_TIMEOUT_SECS").with_default(30),
      Config::integer("MAX_IN_FLIGHT")
        .map(Some)
        .with_default(None),
    ))
  }

  /// No load shedding: the server admits every request, however many are in flight.
  ///
  /// **Re-opens unbounded in-flight requests** (finding M5 of the 0.11 security review): a burst
  /// of slow requests queues work and memory without limit instead of being answered `503`.
  /// Only for a server behind a proxy that caps concurrency itself. Recorded in the posture
  /// registry ([`id_effect::observability::posture`]).
  #[must_use]
  pub fn dangerous_no_load_shedding(mut self) -> Self {
    record_dangerous(
      "id_effect_axum::HostConfig::dangerous_no_load_shedding",
      "M5: no in-flight request cap; overload queues work and memory without bound",
    );
    self.load_shed = LoadShed::disabled();
    self
  }

  /// The TLS and HTTP/3 descriptor: `TLS_CERT_FILE` and `TLS_KEY_FILE` (both or neither;
  /// default none), `TLS_RELOAD_SECS` (default `60`; `0` never re-reads the files) and `HTTP3`
  /// (default `true`).
  #[inline]
  pub fn tls_descriptor() -> Config<(Option<String>, Option<String>, i64, bool)> {
    Config::all((
      Config::optional_string("TLS_CERT_FILE"),
      Config::optional_string("TLS_KEY_FILE"),
      Config::integer("TLS_RELOAD_SECS").with_default(60),
      Config::boolean("HTTP3").with_default(true),
    ))
  }

  /// The validated descriptor (port in `1..=65535`, non-negative timeout, `MAX_IN_FLIGHT` at
  /// least 1, both TLS files or neither, non-negative `TLS_RELOAD_SECS`).
  #[inline]
  pub fn config() -> Config<HostConfig> {
    Config::all((Self::descriptor(), Self::tls_descriptor())).map_attempt(Self::from_parts)
  }

  #[allow(clippy::result_large_err, clippy::type_complexity)]
  fn from_parts(
    (base, tls): (
      (String, i64, i64, Option<i64>),
      (Option<String>, Option<String>, i64, bool),
    ),
  ) -> Result<Self, ConfigError> {
    let mut config = Self::from_tuple(base)?;
    let (tls, http3) = Self::tls_from_tuple(tls)?;
    config.tls = tls;
    config.http3 = http3;
    Ok(config)
  }

  /// This config serving TLS with `tls`.
  #[must_use]
  pub fn with_tls(mut self, tls: HostTls) -> Self {
    self.tls = Some(tls);
    self
  }

  /// What the prod posture warns about in this config (empty when nothing): HTTP/3 enabled
  /// (feature `http3` and [`Self::http3`]) without [`Self::tls`]
  /// ([`HTTP3_WITHOUT_TLS_WARNING`]). The host logs them when it starts.
  pub fn posture_warnings(&self) -> Vec<&'static str> {
    let mut warnings = Vec::new();
    if cfg!(feature = "http3") && self.http3 && self.tls.is_none() {
      warnings.push(HTTP3_WITHOUT_TLS_WARNING);
    }
    warnings
  }

  #[allow(clippy::result_large_err)]
  fn tls_from_tuple(
    (cert, key, reload_secs, http3): (Option<String>, Option<String>, i64, bool),
  ) -> Result<(Option<HostTls>, bool), ConfigError> {
    let Ok(reload_secs) = u64::try_from(reload_secs) else {
      return Err(ConfigError::Invalid {
        path: "TLS_RELOAD_SECS".into(),
        value: reload_secs.to_string(),
        reason: "the reload interval must be non-negative".into(),
      });
    };
    let tls = match (cert, key) {
      (None, None) => None,
      (Some(cert_file), Some(key_file)) => Some(HostTls::Files {
        cert_file: cert_file.into(),
        key_file: key_file.into(),
        reload_every: (reload_secs > 0).then(|| Duration::from_secs(reload_secs)),
      }),
      (cert, _) => {
        let missing = if cert.is_none() {
          "TLS_CERT_FILE"
        } else {
          "TLS_KEY_FILE"
        };
        return Err(ConfigError::Invalid {
          path: missing.into(),
          value: String::new(),
          reason: "set both TLS_CERT_FILE and TLS_KEY_FILE, or neither".into(),
        });
      }
    };
    Ok((tls, http3))
  }

  /// A layer providing [`HostConfig`] from the [`ConfigProviderService`] capability (feed it a
  /// provider layer such as [`provide_host_config_env`] with `provide_to`).
  #[inline]
  pub fn layer<E>() -> Layer<HCons<HostConfig, HNil>, E, ConfigEnv>
  where
    E: From<ConfigError> + Send + 'static,
  {
    Self::config().layer()
  }

  #[allow(clippy::result_large_err)]
  fn from_tuple(
    (bind_host, port, timeout_secs, max_in_flight): (String, i64, i64, Option<i64>),
  ) -> Result<Self, ConfigError> {
    if !(1..=i64::from(u16::MAX)).contains(&port) {
      return Err(ConfigError::Invalid {
        path: "PORT".into(),
        value: port.to_string(),
        reason: "port must be between 1 and 65535".into(),
      });
    }
    let Ok(timeout_secs) = u64::try_from(timeout_secs) else {
      return Err(ConfigError::Invalid {
        path: "SHUTDOWN_TIMEOUT_SECS".into(),
        value: timeout_secs.to_string(),
        reason: "timeout must be non-negative".into(),
      });
    };
    let load_shed = match max_in_flight {
      None => LoadShed::default(),
      Some(n) => match usize::try_from(n) {
        Ok(n) if n >= 1 => LoadShed::new(n),
        _ => {
          return Err(ConfigError::Invalid {
            path: "MAX_IN_FLIGHT".into(),
            value: n.to_string(),
            reason: "the in-flight cap must be at least 1".into(),
          });
        }
      },
    };
    Ok(Self {
      bind_host,
      bind_port: u16::try_from(port).unwrap_or(u16::MAX),
      shutdown_timeout: Duration::from_secs(timeout_secs),
      load_shed,
      ..Self::default()
    })
  }
}

/// Load [`HostConfig`] using the [`ConfigProviderService`] capability in `R` (index `I` inferred).
#[inline]
pub fn load_host_config<R, I>() -> Effect<HostConfig, ConfigError, R>
where
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  HostConfig::config().run::<HostConfig, ConfigError, R, I>()
}

/// A layer providing the process environment as the config provider.
#[inline]
pub fn provide_host_config_env<E: Send + 'static>() -> Layer<ConfigEnv, E> {
  EnvConfigProviderLive::layer()
}

/// A minimal environment with only `provider` as the config provider (tests / CLI bootstrap).
#[inline]
pub fn bootstrap_env<P: ConfigProvider + 'static>(provider: P) -> ConfigEnv {
  config_env(provider)
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Exit;
  use id_effect::need;
  use id_effect_config::{MapConfigProvider, provide_config_provider};

  #[allow(clippy::result_large_err)]
  fn load(pairs: &[(&str, &str)]) -> Result<HostConfig, id_effect::Cause<ConfigError>> {
    let env = bootstrap_env(MapConfigProvider::from_pairs(pairs.iter().copied()));
    load_host_config().provide_all(env).block_on().into_result()
  }

  #[test]
  fn load_host_config_uses_defaults() {
    let cfg = load(&[]).unwrap();
    assert_eq!(cfg.bind_host, "0.0.0.0");
    assert_eq!(cfg.bind_port, 8080);
    assert_eq!(cfg.shutdown_timeout, Duration::from_secs(30));
    assert_eq!(cfg.load_shed, LoadShed::default());
    assert_eq!(cfg, HostConfig::default());
  }

  #[test]
  fn load_host_config_reads_and_validates_max_in_flight() {
    let cfg = load(&[("MAX_IN_FLIGHT", "64")]).unwrap();
    assert_eq!(cfg.load_shed.max_in_flight(), Some(64));
    assert!(load(&[("MAX_IN_FLIGHT", "0")]).is_err());
    assert!(load(&[("MAX_IN_FLIGHT", "-3")]).is_err());
  }

  #[test]
  fn dangerous_no_load_shedding_disables_and_records() {
    let cfg = HostConfig::default().dangerous_no_load_shedding();
    assert_eq!(cfg.load_shed.max_in_flight(), None);
    assert!(
      id_effect::observability::posture::dangerous_opt_outs()
        .iter()
        .any(|(id, _)| *id == "id_effect_axum::HostConfig::dangerous_no_load_shedding")
    );
  }

  #[test]
  fn load_host_config_reads_overrides() {
    let cfg = load(&[
      ("HOST", "127.0.0.1"),
      ("PORT", "9090"),
      ("SHUTDOWN_TIMEOUT_SECS", "5"),
    ])
    .unwrap();
    assert_eq!(cfg.bind_host, "127.0.0.1");
    assert_eq!(cfg.bind_port, 9090);
    assert_eq!(cfg.shutdown_timeout, Duration::from_secs(5));
  }

  #[test]
  fn load_host_config_reads_tls_files_and_http3() {
    let cfg = load(&[]).unwrap();
    assert_eq!(cfg.tls, None);
    assert!(cfg.http3);
    let cfg = load(&[
      ("TLS_CERT_FILE", "/etc/tls/chain.pem"),
      ("TLS_KEY_FILE", "/etc/tls/key.pem"),
      ("HTTP3", "false"),
    ])
    .unwrap();
    assert_eq!(
      cfg.tls,
      Some(HostTls::files("/etc/tls/chain.pem", "/etc/tls/key.pem"))
    );
    assert!(!cfg.http3);
    let fixed = load(&[
      ("TLS_CERT_FILE", "c.pem"),
      ("TLS_KEY_FILE", "k.pem"),
      ("TLS_RELOAD_SECS", "0"),
    ])
    .unwrap();
    assert_eq!(
      fixed.tls,
      Some(HostTls::Files {
        cert_file: "c.pem".into(),
        key_file: "k.pem".into(),
        reload_every: None,
      })
    );
    let every_5 = load(&[
      ("TLS_CERT_FILE", "c.pem"),
      ("TLS_KEY_FILE", "k.pem"),
      ("TLS_RELOAD_SECS", "5"),
    ])
    .unwrap();
    assert!(matches!(
      every_5.tls,
      Some(HostTls::Files { reload_every: Some(d), .. }) if d == Duration::from_secs(5)
    ));
  }

  #[test]
  fn load_host_config_rejects_half_a_tls_config() {
    let only_cert = load(&[("TLS_CERT_FILE", "c.pem")]).unwrap_err();
    assert!(
      format!("{only_cert:?}").contains("TLS_KEY_FILE"),
      "{only_cert:?}"
    );
    let only_key = load(&[("TLS_KEY_FILE", "k.pem")]).unwrap_err();
    assert!(
      format!("{only_key:?}").contains("TLS_CERT_FILE"),
      "{only_key:?}"
    );
    let negative = load(&[("TLS_RELOAD_SECS", "-1")]).unwrap_err();
    assert!(
      format!("{negative:?}").contains("TLS_RELOAD_SECS"),
      "{negative:?}"
    );
  }

  #[test]
  fn posture_warns_about_http3_without_tls_only() {
    let expected: Vec<&str> = if cfg!(feature = "http3") {
      vec![HTTP3_WITHOUT_TLS_WARNING]
    } else {
      Vec::new()
    };
    assert_eq!(HostConfig::default().posture_warnings(), expected);
    let with_tls = HostConfig::default().with_tls(HostTls::files("c.pem", "k.pem"));
    assert!(with_tls.posture_warnings().is_empty());
    let off = HostConfig {
      http3: false,
      ..HostConfig::default()
    };
    assert!(off.posture_warnings().is_empty());
  }

  #[test]
  fn load_host_config_rejects_invalid_port() {
    assert!(load(&[("PORT", "99999")]).is_err());
  }

  #[test]
  fn load_host_config_rejects_negative_shutdown_timeout() {
    assert!(load(&[("SHUTDOWN_TIMEOUT_SECS", "-1")]).is_err());
  }

  #[test]
  fn host_config_layer_provides_the_capability() {
    let app = provide_config_provider(MapConfigProvider::from_pairs([("PORT", "7070")]))
      .provide_to(HostConfig::layer::<ConfigError>());
    let port: Effect<u16, ConfigError, HCons<HostConfig, HNil>> =
      need::<HostConfig>().map(|c| c.bind_port);
    assert!(matches!(
      port.provide_layer(app).block_on(),
      Exit::Success(7070)
    ));
    // The env-backed provider layer builds too.
    let env_app = provide_host_config_env::<ConfigError>().provide_to(HostConfig::layer());
    let any: Effect<(), ConfigError, HCons<HostConfig, HNil>> = need::<HostConfig>().map(|_| ());
    let _ = any.provide_layer(env_app).block_on();
  }
}
