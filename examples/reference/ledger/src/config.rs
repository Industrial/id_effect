//! Configuration per profile (`dev`, `test`, `prod`), lowest precedence first:
//!
//! 1. `config/base.toml`, then `config/<profile>.toml`;
//! 2. the Vault secrets, when the profile has a `[vault]` section (`prod`): each secret path is
//!    merged under its key (`ledger/db`'s `password` becomes `db.password`);
//! 3. `LEDGER_*` environment variables (`LEDGER_DB__HOST=db` sets `db.host`).
//!
//! The profile comes from `--profile <name>`, else `ID_EFFECT_PROFILE`, else `dev`. Secrets are
//! [`Secret`]s from the moment they are read, so no `Debug` output shows them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use id_effect::{Effect, HNil, fail, succeed};
use id_effect_kit::config::{ConfigError, Profile, Profiles, Secret, VaultAuth, VaultSettings};
use serde::Deserialize;

/// Prefix of the environment variables merged last.
pub const ENV_PREFIX: &str = "LEDGER_";

/// The profiles under `dir` (`config/` in the repository and in the image).
pub fn profiles(dir: impl Into<PathBuf>) -> Profiles {
  Profiles::new().dir(dir).env_prefix(ENV_PREFIX)
}

/// The whole configuration of one profile.
#[derive(Clone, Debug, Deserialize)]
pub struct LedgerConfig {
  /// The HTTP server and its limits.
  pub http: HttpConfig,
  /// The primary database, its replicas and the migrations.
  pub db: DbConfig,
  /// The balance cache and the login rate limit; in process when absent.
  #[serde(default)]
  pub redis: RedisConfig,
  /// Passwords, sessions, tokens and TOTP.
  pub auth: AuthConfig,
  /// OpenID Connect login; off when absent.
  pub oidc: Option<OidcConfig>,
  /// Where the secrets are (prod).
  pub vault: Option<VaultConfig>,
}

/// `[http]`.
#[derive(Clone, Debug, Deserialize)]
pub struct HttpConfig {
  /// Interface to bind.
  pub host: String,
  /// Port to bind.
  pub port: u16,
  /// Requests running longer are answered `503` (their fiber interrupted).
  pub request_timeout_ms: u64,
  /// Larger bodies are answered `413`.
  pub body_limit_bytes: usize,
}

/// `[db]`.
#[derive(Clone, Debug, Deserialize)]
pub struct DbConfig {
  /// Host of the primary.
  pub host: String,
  /// Port of the primary.
  pub port: u16,
  /// Login role.
  pub user: String,
  /// Password (from Vault in `prod`).
  pub password: Secret<String>,
  /// Database name.
  pub name: String,
  /// The migrations directory, applied at start.
  pub migrations: String,
  /// Read replicas (PostgreSQL URLs).
  #[serde(default)]
  pub replicas: Vec<Secret<String>>,
  /// Pool size per database.
  pub max_connections: u32,
}

impl DbConfig {
  /// The primary's URL, credentials percent-encoded.
  pub fn url(&self) -> Secret<String> {
    Secret::new(format!(
      "postgres://{}:{}@{}:{}/{}",
      encode(&self.user),
      encode(self.password.expose()),
      self.host,
      self.port,
      encode(&self.name)
    ))
  }
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
fn encode(raw: &str) -> String {
  raw
    .bytes()
    .map(|b| match b {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
        (b as char).to_string()
      }
      _ => format!("%{b:02X}"),
    })
    .collect()
}

/// `[redis]`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RedisConfig {
  /// `redis://host:port/db`; none: the cache and the rate limit stay in process.
  pub url: Option<String>,
}

/// `[auth]`.
#[derive(Clone, Debug, Deserialize)]
pub struct AuthConfig {
  /// HS256 secret of the API tokens (32+ random bytes; from Vault in `prod`).
  pub jwt_secret: Secret<String>,
  /// `iss` of the API tokens.
  pub jwt_issuer: String,
  /// `aud` of the API tokens.
  pub jwt_audience: String,
  /// Lifetime of an API token.
  pub token_ttl_minutes: u64,
  /// Name of the session cookie (`__Host-` prefixed in prod).
  pub session_cookie: String,
  /// Whether cookies are `Secure` (HTTPS only).
  pub secure_cookies: bool,
  /// Login attempts refilled per second, per client.
  pub login_per_second: f64,
  /// Login attempts allowed in a burst.
  pub login_burst: f64,
  /// Argon2id memory cost (KiB).
  pub password_memory_kib: u32,
  /// Issuer shown by authenticator apps.
  pub totp_issuer: String,
}

/// `[oidc]`.
#[derive(Clone, Debug, Deserialize)]
pub struct OidcConfig {
  /// The provider's issuer URL (`…/realms/ledger` on Keycloak).
  pub issuer: String,
  /// The client registered at the provider.
  pub client_id: String,
  /// Its secret.
  pub client_secret: Secret<String>,
  /// `…/auth/oidc/callback` on this service, as registered.
  pub redirect_url: String,
}

/// `[vault]`.
#[derive(Clone, Debug, Deserialize)]
pub struct VaultConfig {
  /// The server (`http://vault:8200`).
  pub addr: String,
  /// KV v2 mount.
  #[serde(default = "default_mount")]
  pub mount: String,
  /// Configuration key → secret path (`db = "ledger/db"`).
  pub secrets: BTreeMap<String, String>,
}

fn default_mount() -> String {
  "secret".to_owned()
}

/// Only the `[vault]` section, read before the rest (which may need its secrets).
#[derive(Deserialize)]
struct Bootstrap {
  vault: Option<VaultConfig>,
}

fn ready<A: Send + 'static>(r: Result<A, ConfigError>) -> Effect<A, ConfigError> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

/// Loads `profile`: the files, then, when the profile has a `[vault]` section, every secret it
/// names, read once with `vault_auth` (or the token in `VAULT_TOKEN`), then the environment.
pub fn load(
  profiles: &Profiles,
  profile: Profile,
  vault_auth: Option<VaultAuth>,
) -> Effect<LedgerConfig, ConfigError> {
  let files = match profiles.load_for(profile.clone()) {
    Ok(files) => files,
    Err(e) => return fail(e),
  };
  let vault = match files.extract::<Bootstrap>() {
    Ok(bootstrap) => bootstrap.vault,
    Err(e) => return fail(e),
  };
  let Some(vault) = vault else {
    return ready(files.extract());
  };
  let auth = match vault_auth.map_or_else(VaultAuth::token_from_env, Ok) {
    Ok(auth) => auth,
    Err(e) => return fail(e),
  };
  let settings = vault.secrets.iter().fold(
    VaultSettings::new(vault.addr.clone(), auth).mount(vault.mount.clone()),
    |settings, (at, path)| settings.path_at(path.clone(), at.clone()),
  );
  let profiles = profiles.clone();
  settings
    .fetch::<ConfigError, HNil>()
    .flat_map(move |secrets| {
      ready(
        profiles
          .overlay(secrets)
          .load_for(profile)
          .and_then(|merged| merged.extract()),
      )
    })
}

#[cfg(test)]
mod tests {
  use super::encode;

  #[test]
  fn credentials_are_percent_encoded() {
    assert_eq!(encode("p@ss:w/rd"), "p%40ss%3Aw%2Frd");
    assert_eq!(encode("plain-pass_1.~"), "plain-pass_1.~");
  }
}
