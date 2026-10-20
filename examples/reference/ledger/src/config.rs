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
use id_effect_kit::platform::http::SsrfPolicy;
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
  /// Outgoing mail: the transport, the sender, the account links and the previews.
  pub mail: MailConfig,
  /// Where the monthly statements are stored (S3).
  pub storage: StorageConfig,
  /// The monthly statements: when the cron fires and how a statement is delivered.
  pub statements: StatementsConfig,
  /// The background workers of this process.
  pub jobs: JobsConfig,
  /// Where postings are announced and who listens.
  pub messaging: MessagingConfig,
  /// The operator console at `/_console`.
  #[serde(default)]
  pub console: ConsoleConfig,
  /// Outbound HTTP (SendGrid): the SSRF policy's allowlist; private networks refused when absent.
  #[serde(default)]
  pub outbound: SsrfPolicy,
  /// The profile this configuration was loaded for (`dev`, `test`, `prod`); set by [`load`],
  /// read by what differs per profile (`/docs` is served in `dev` and `test` only).
  #[serde(skip)]
  pub profile: String,
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
  /// The reverse proxies in front of the service (CIDRs), whose `X-Forwarded-For` entries name
  /// the client the login limit counts against; empty (the default): the connection's peer is
  /// the client and the header is ignored.
  #[serde(default)]
  pub trusted_proxies: Vec<String>,
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
  /// Base64 of 64+ random bytes: signs the CSRF cookie (from Vault in `prod`; every instance
  /// needs the same key).
  pub cookie_key: Secret<String>,
  /// Name of the session cookie (`__Host-` prefixed in prod).
  pub session_cookie: String,
  /// Whether cookies are `Secure` (HTTPS only).
  pub secure_cookies: bool,
  /// Login attempts refilled per second, per client.
  pub login_per_second: f64,
  /// Login attempts allowed in a burst.
  pub login_burst: f64,
  /// Login attempts refilled per second, per organisation (all its clients together).
  pub login_tenant_per_second: f64,
  /// Login attempts an organisation may make in a burst.
  pub login_tenant_burst: f64,
  /// Failed password logins an account or a client address may make before each further one
  /// waits (1 s, doubling up to 15 minutes; `id_effect_auth::LoginThrottle`).
  #[serde(default = "default_login_free_attempts")]
  pub login_free_attempts: u32,
  /// How often the profile file is polled for new login limits (`0`: never).
  #[serde(default)]
  pub reload_limits_secs: u64,
  /// Argon2id memory cost (KiB).
  pub password_memory_kib: u32,
  /// Issuer shown by authenticator apps.
  pub totp_issuer: String,
}

/// `[mail]`.
#[derive(Clone, Debug, Deserialize)]
pub struct MailConfig {
  /// How mail leaves the service.
  pub transport: MailTransport,
  /// The sender's address.
  pub from: String,
  /// The sender's display name.
  pub from_name: String,
  /// The mail templates (`<name>.subject.txt`, `<name>.txt`, `<name>.html`).
  pub templates: String,
  /// SMTP server (`transport = "smtp"`).
  #[serde(default)]
  pub smtp_host: String,
  /// SMTP port (plaintext; Mailpit in dev).
  #[serde(default)]
  pub smtp_port: u16,
  /// SendGrid API key (`transport = "sendgrid"`; from Vault in `prod`).
  pub sendgrid_api_key: Option<Secret<String>>,
  /// SendGrid API base URL (default the public API).
  pub sendgrid_base_url: Option<String>,
  /// The page an email verification link opens (`?token=` is appended).
  pub verify_url: String,
  /// The page a password reset link opens (`?token=` is appended).
  pub reset_url: String,
  /// Serve `/_mail/previews` (the dev profile only).
  #[serde(default)]
  pub previews: bool,
}

/// `mail.transport`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailTransport {
  /// Kept in memory, never sent (tests).
  Memory,
  /// Plain SMTP (Mailpit in dev).
  Smtp,
  /// The SendGrid v3 API over traced HTTP (prod).
  Sendgrid,
}

/// `[storage]`.
#[derive(Clone, Debug, Deserialize)]
pub struct StorageConfig {
  /// The S3 bucket of the statements; in memory when absent.
  pub bucket: Option<String>,
  /// An S3-compatible endpoint (`http://localhost:8333`); AWS when absent.
  pub endpoint: Option<String>,
  /// The region.
  pub region: String,
  /// Access key id; the environment's AWS credentials when absent.
  pub access_key_id: Option<String>,
  /// Secret access key.
  pub secret_access_key: Option<Secret<String>>,
  /// Allow plain HTTP to the endpoint (dev).
  #[serde(default)]
  pub allow_http: bool,
}

/// `[statements]`.
#[derive(Clone, Debug, Deserialize)]
pub struct StatementsConfig {
  /// When the statements of the previous month are due (six-field cron, UTC).
  pub cron: String,
  /// A statement CSV up to this size is attached to its mail; a larger one is linked.
  pub attach_max_bytes: usize,
  /// How long the signed link of a linked statement is valid.
  pub link_ttl_hours: u64,
}

/// `[jobs]`.
#[derive(Clone, Debug, Deserialize)]
pub struct JobsConfig {
  /// Run the worker, the statement cron, the relay and the consumers in this process.
  pub run: bool,
  /// Jobs of one queue running at a time.
  pub concurrency: u16,
}

/// `[messaging]`.
#[derive(Clone, Debug, Deserialize)]
pub struct MessagingConfig {
  /// The topic every posting is announced on.
  pub topic: String,
  /// The consumer group of the balance notifications.
  pub group: String,
  /// A notification is dead-lettered after this many failed deliveries.
  pub max_deliveries: u32,
  /// This instance's consumer name on Redis Streams (stable per instance).
  pub consumer: String,
}

/// `[console]`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConsoleConfig {
  /// Mount `/_console` (behind `console.admin`).
  #[serde(default)]
  pub enabled: bool,
}

/// The login limits of `[auth]`, as the profile file is reloaded ([`crate::limits`]).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct LoginLimits {
  /// Per client: tokens per second.
  pub login_per_second: f64,
  /// Per client: burst.
  pub login_burst: f64,
  /// Per organisation: tokens per second.
  pub login_tenant_per_second: f64,
  /// Per organisation: burst.
  pub login_tenant_burst: f64,
}

impl AuthConfig {
  /// The login limits in force.
  pub fn limits(&self) -> LoginLimits {
    LoginLimits {
      login_per_second: self.login_per_second,
      login_burst: self.login_burst,
      login_tenant_per_second: self.login_tenant_per_second,
      login_tenant_burst: self.login_tenant_burst,
    }
  }
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

fn default_login_free_attempts() -> u32 {
  5
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
  let name = profile.as_str().to_owned();
  let Some(vault) = vault else {
    return ready(files.extract()).map(named(name));
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
    .map(named(name))
}

/// Records the profile `name` in the loaded configuration.
fn named(name: String) -> impl FnOnce(LedgerConfig) -> LedgerConfig + Send + 'static {
  move |config| LedgerConfig {
    profile: name,
    ..config
  }
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
