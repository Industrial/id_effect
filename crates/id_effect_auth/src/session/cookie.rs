//! The session cookie: `Set-Cookie` rendering and lookup in a `Cookie` header.

use std::fmt;
use std::time::Duration;

use super::SessionId;

/// The cookie's `SameSite` attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SameSite {
  /// Sent only on same-site requests.
  Strict,
  /// Also sent on top-level cross-site navigations (links); the default.
  Lax,
  /// Sent on every request; browsers require `Secure` with it.
  None,
}

impl fmt::Display for SameSite {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::Strict => "Strict",
      Self::Lax => "Lax",
      Self::None => "None",
    })
  }
}

/// How the session id travels: the cookie's name and attributes.
///
/// The default is the most restrictive cookie a login session can use: the `__Host-` name
/// prefix (browsers then require `Secure`, `Path=/` and no `Domain`, so a sibling subdomain
/// cannot plant or overwrite it), `Secure`, `HttpOnly` (scripts cannot read it) and
/// `SameSite=Lax`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookieConfig {
  /// Cookie name (default `__Host-id_session`).
  pub name: String,
  /// `Path` (default `/`).
  pub path: String,
  /// `Domain`; must stay `None` with a `__Host-` name (default `None`).
  pub domain: Option<String>,
  /// `Secure`: HTTPS only (default `true`; turn off only for plain-HTTP local development, with
  /// a name without the `__Host-` prefix).
  pub secure: bool,
  /// `HttpOnly` (default `true`).
  pub http_only: bool,
  /// `SameSite` (default [`SameSite::Lax`]).
  pub same_site: SameSite,
}

impl Default for CookieConfig {
  fn default() -> Self {
    Self {
      name: "__Host-id_session".to_owned(),
      path: "/".to_owned(),
      domain: None,
      secure: true,
      http_only: true,
      same_site: SameSite::Lax,
    }
  }
}

impl CookieConfig {
  /// A `Set-Cookie` header value carrying `id` for `max_age`.
  pub fn set_cookie(&self, id: &SessionId, max_age: Duration) -> String {
    self.render(id.as_str(), max_age.as_secs())
  }

  /// A `Set-Cookie` header value that deletes the cookie (logout).
  pub fn clear_cookie(&self) -> String {
    self.render("", 0)
  }

  fn render(&self, value: &str, max_age: u64) -> String {
    let mut out = format!(
      "{}={value}; Path={}; Max-Age={max_age}",
      self.name, self.path
    );
    if let Some(domain) = &self.domain {
      out.push_str("; Domain=");
      out.push_str(domain);
    }
    if self.secure {
      out.push_str("; Secure");
    }
    if self.http_only {
      out.push_str("; HttpOnly");
    }
    out.push_str("; SameSite=");
    out.push_str(&self.same_site.to_string());
    out
  }

  /// The session id in a `Cookie` request header value (`a=1; name=<id>`), if one is present
  /// and well formed. With duplicate names the first well-formed value wins.
  pub fn find(&self, cookie_header: &str) -> Option<SessionId> {
    cookie_header
      .split(';')
      .filter_map(|pair| pair.trim().split_once('='))
      .filter(|(name, _)| *name == self.name)
      .find_map(|(_, value)| SessionId::parse(value.trim_matches('"')))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_cookie_is_host_prefixed_secure_http_only_lax() {
    let id = SessionId::generate().unwrap();
    let header = CookieConfig::default().set_cookie(&id, Duration::from_secs(1800));
    assert_eq!(
      header,
      format!(
        "__Host-id_session={}; Path=/; Max-Age=1800; Secure; HttpOnly; SameSite=Lax",
        id.as_str()
      )
    );
  }

  #[test]
  fn clear_cookie_expires_it_now() {
    let header = CookieConfig::default().clear_cookie();
    assert!(
      header.starts_with("__Host-id_session=; Path=/; Max-Age=0;"),
      "{header}"
    );
  }

  #[test]
  fn optional_attributes_render() {
    let cfg = CookieConfig {
      name: "sid".into(),
      domain: Some("example.com".into()),
      secure: false,
      http_only: false,
      same_site: SameSite::Strict,
      ..CookieConfig::default()
    };
    let id = SessionId::generate().unwrap();
    let header = cfg.set_cookie(&id, Duration::from_secs(5));
    assert!(
      header.ends_with("; Domain=example.com; SameSite=Strict"),
      "{header}"
    );
    assert!(!header.contains("Secure") && !header.contains("HttpOnly"));
    assert_eq!(SameSite::None.to_string(), "None");
  }

  #[test]
  fn find_reads_the_id_from_a_cookie_header() {
    let cfg = CookieConfig::default();
    let id = SessionId::generate().unwrap();
    let header = format!("theme=dark; __Host-id_session={}; other=1", id.as_str());
    assert_eq!(cfg.find(&header), Some(id.clone()));
    let quoted = format!("__Host-id_session=\"{}\"", id.as_str());
    assert_eq!(cfg.find(&quoted), Some(id.clone()));
    // Malformed values are skipped, a later well-formed duplicate still counts.
    let dup = format!("__Host-id_session=junk; __Host-id_session={}", id.as_str());
    assert_eq!(cfg.find(&dup), Some(id));
    assert_eq!(cfg.find("theme=dark"), None);
    assert_eq!(cfg.find(""), None);
    assert_eq!(cfg.find("__Host-id_session=nope"), None);
  }
}
