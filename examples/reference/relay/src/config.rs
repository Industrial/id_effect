//! The binary's settings, from the environment ([`Settings::from_lookup`] takes any lookup, so
//! it is tested without touching the process environment).
//!
//! | Variable | Meaning |
//! |---|---|
//! | `RELAY_NODE` | this node's name, `name@host` (required) |
//! | `RELAY_BIND` | TCP and UDP address, one port (default `0.0.0.0:8443`) |
//! | `RELAY_PEERS` | comma-separated addresses of the nodes to join (default none) |
//! | `TLS_CERT_FILE`, `TLS_KEY_FILE` | the certificate served to clients, PEM (required) |
//! | `RELAY_MESH_CERT_FILE`, `RELAY_MESH_KEY_FILE` | this node's mesh certificate, PEM (required) |
//! | `RELAY_MESH_TRUSTED` | comma-separated SHA-256 (hex) of every node's mesh certificate (required) |
//! | `RELAY_MESH_COOKIE` | the mesh's shared cookie, at least 32 bytes (required) |
//! | `RELAY_ORIGINS` | comma-separated origins of the web client (default none: browsers refused) |
//! | `RELAY_TOKEN_SECRET`, `RELAY_TOKEN_ISSUER` | how bearer tokens are verified (HS256, required) |
//! | `RELAY_DATABASE_URL` | PostgreSQL (required) |

use std::net::SocketAddr;

/// The default address: every interface, port 8443 (TCP and UDP).
pub const DEFAULT_BIND: &str = "0.0.0.0:8443";

/// The binary's settings.
#[derive(Clone, PartialEq, Eq)]
pub struct Settings {
  /// `RELAY_NODE`.
  pub node: String,
  /// `RELAY_BIND`.
  pub bind: SocketAddr,
  /// `RELAY_PEERS`.
  pub peers: Vec<SocketAddr>,
  /// `TLS_CERT_FILE`, `TLS_KEY_FILE`.
  pub tls: (String, String),
  /// `RELAY_MESH_CERT_FILE`, `RELAY_MESH_KEY_FILE`.
  pub mesh_tls: (String, String),
  /// `RELAY_MESH_TRUSTED`.
  pub mesh_trusted: Vec<[u8; 32]>,
  /// `RELAY_MESH_COOKIE`.
  pub mesh_cookie: String,
  /// `RELAY_ORIGINS`.
  pub origins: Vec<String>,
  /// `RELAY_TOKEN_SECRET`.
  pub token_secret: String,
  /// `RELAY_TOKEN_ISSUER`.
  pub token_issuer: String,
  /// `RELAY_DATABASE_URL`.
  pub database_url: String,
}

impl std::fmt::Debug for Settings {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Settings")
      .field("node", &self.node)
      .field("bind", &self.bind)
      .field("peers", &self.peers)
      .field("origins", &self.origins)
      .finish_non_exhaustive()
  }
}

impl Settings {
  /// The settings from the process environment.
  ///
  /// # Errors
  ///
  /// The first missing or malformed variable.
  pub fn from_env() -> Result<Self, String> {
    Self::from_lookup(|name| std::env::var(name).ok())
  }

  /// The settings from `lookup` (variable name to value).
  ///
  /// # Errors
  ///
  /// The first missing or malformed variable.
  pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
    let required = |name: &str| {
      lookup(name)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{name} is not set"))
    };
    let list = |name: &str| -> Vec<String> {
      lookup(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
    };
    let bind = lookup("RELAY_BIND").unwrap_or_else(|| DEFAULT_BIND.to_owned());
    let bind = bind
      .trim()
      .parse()
      .map_err(|e| format!("RELAY_BIND: {e}"))?;
    let peers = list("RELAY_PEERS")
      .iter()
      .map(|p| p.parse().map_err(|e| format!("RELAY_PEERS: {p}: {e}")))
      .collect::<Result<_, _>>()?;
    let mesh_trusted = list("RELAY_MESH_TRUSTED")
      .iter()
      .map(|hex| parse_sha256(hex).ok_or_else(|| format!("RELAY_MESH_TRUSTED: {hex}")))
      .collect::<Result<Vec<_>, _>>()?;
    if mesh_trusted.is_empty() {
      return Err("RELAY_MESH_TRUSTED is not set".to_owned());
    }
    Ok(Self {
      node: required("RELAY_NODE")?,
      bind,
      peers,
      tls: (required("TLS_CERT_FILE")?, required("TLS_KEY_FILE")?),
      mesh_tls: (
        required("RELAY_MESH_CERT_FILE")?,
        required("RELAY_MESH_KEY_FILE")?,
      ),
      mesh_trusted,
      mesh_cookie: required("RELAY_MESH_COOKIE")?,
      origins: list("RELAY_ORIGINS"),
      token_secret: required("RELAY_TOKEN_SECRET")?,
      token_issuer: required("RELAY_TOKEN_ISSUER")?,
      database_url: required("RELAY_DATABASE_URL")?,
    })
  }
}

/// A SHA-256 written as 64 hex digits (`:` separators allowed).
pub fn parse_sha256(text: &str) -> Option<[u8; 32]> {
  let digits: Vec<u8> = text.bytes().filter(|b| *b != b':').collect();
  if digits.len() != 64 {
    return None;
  }
  let mut out = [0_u8; 32];
  for (i, pair) in digits.chunks(2).enumerate() {
    let pair = std::str::from_utf8(pair).ok()?;
    out[i] = u8::from_str_radix(pair, 16).ok()?;
  }
  Some(out)
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;

  use super::*;

  fn complete() -> HashMap<&'static str, String> {
    HashMap::from([
      ("RELAY_NODE", "a@relay".to_owned()),
      ("RELAY_PEERS", "10.0.0.2:8443, 10.0.0.3:8443".to_owned()),
      ("TLS_CERT_FILE", "/tls/cert.pem".to_owned()),
      ("TLS_KEY_FILE", "/tls/key.pem".to_owned()),
      ("RELAY_MESH_CERT_FILE", "/mesh/cert.pem".to_owned()),
      ("RELAY_MESH_KEY_FILE", "/mesh/key.pem".to_owned()),
      (
        "RELAY_MESH_TRUSTED",
        format!("{},{}", "ab".repeat(32), "0".repeat(64)),
      ),
      ("RELAY_MESH_COOKIE", "c".repeat(32)),
      ("RELAY_ORIGINS", "https://chat.example".to_owned()),
      ("RELAY_TOKEN_SECRET", "s".repeat(32)),
      ("RELAY_TOKEN_ISSUER", "https://id.example".to_owned()),
      ("RELAY_DATABASE_URL", "postgres://relay@db/relay".to_owned()),
    ])
  }

  fn load(vars: &HashMap<&'static str, String>) -> Result<Settings, String> {
    Settings::from_lookup(|name| vars.get(name).cloned())
  }

  #[test]
  fn settings_load_from_the_environment() {
    let settings = load(&complete()).unwrap();
    assert_eq!(settings.node, "a@relay");
    assert_eq!(settings.bind, DEFAULT_BIND.parse().unwrap());
    assert_eq!(settings.peers.len(), 2);
    assert_eq!(settings.mesh_trusted[0], [0xab; 32]);
    assert_eq!(settings.origins, ["https://chat.example"]);
    assert!(!format!("{settings:?}").contains("postgres://"));
  }

  #[test]
  fn missing_and_malformed_settings_are_named() {
    for var in [
      "RELAY_NODE",
      "RELAY_TOKEN_SECRET",
      "RELAY_DATABASE_URL",
      "RELAY_MESH_TRUSTED",
    ] {
      let mut vars = complete();
      vars.remove(var);
      assert!(load(&vars).unwrap_err().contains(var), "{var}");
    }
    let mut vars = complete();
    vars.insert("RELAY_BIND", "nowhere".to_owned());
    assert!(load(&vars).unwrap_err().starts_with("RELAY_BIND"));
    let mut vars = complete();
    vars.insert("RELAY_PEERS", "x".to_owned());
    assert!(load(&vars).unwrap_err().starts_with("RELAY_PEERS"));
    let mut vars = complete();
    vars.insert("RELAY_MESH_TRUSTED", "abc".to_owned());
    assert!(load(&vars).unwrap_err().starts_with("RELAY_MESH_TRUSTED"));
  }

  #[test]
  fn fingerprints_parse_with_or_without_colons() {
    let colons = vec!["0f"; 32].join(":");
    assert_eq!(parse_sha256(&colons), Some([0x0f; 32]));
    assert_eq!(parse_sha256(&"zz".repeat(32)), None);
    assert_eq!(parse_sha256("00"), None);
  }
}
