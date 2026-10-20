//! The `Origin` allowlist checked on every WebSocket upgrade (ADR 0013 R9, ADR 0014 C4).
//!
//! A browser sends `Origin` on every WebSocket handshake, and cookies ride along whatever page
//! opened the socket: without a check, any site the user visits can open an authenticated
//! socket to the service (cross-site WebSocket hijacking). [`OriginPolicy`] is the check:
//!
//! - **Empty by default**, and an empty list refuses every browser origin.
//! - Entries are exact origins (`https://app.example.com`, `https://app.example.com:8443`) or an
//!   origin with any port (`http://localhost:*`), compared case-insensitively on scheme and host,
//!   with the scheme's default port filled in (`https://a.example` is `https://a.example:443`).
//!   `null` (sandboxed frames, `file:` pages) matches only an explicit `null` entry.
//! - A request **without** `Origin` comes from a non-browser client (a CLI, a service, the RPC
//!   client in Rust) and passes; it still goes through the route's guard. A request with two
//!   `Origin` headers, or one that is not an origin, is refused.
//! - [`OriginPolicy::dangerous_any_origin`] admits every origin and records the opt-out in the
//!   [posture registry](id_effect::observability::posture).
//!
//! The development profile allows the local origins ([`DEV_ORIGINS`]) by configuration, not by
//! an opt-out: put them in the dev profile's allowlist and nowhere else.
//!
//! ```rust
//! use id_effect_axum::realtime::origin::OriginPolicy;
//!
//! let policy = OriginPolicy::allow(["https://app.example.com", "http://localhost:*"]).unwrap();
//! assert!(policy.allows("https://APP.example.com:443"));
//! assert!(policy.allows("http://localhost:5173"));
//! assert!(!policy.allows("https://evil.example"));
//! assert!(!OriginPolicy::deny_all().allows("https://app.example.com"));
//! ```

use std::fmt;

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::route::Problem;

/// The local origins a development profile allows by configuration (any port on `localhost`,
/// `127.0.0.1` and `[::1]`, plain HTTP).
pub const DEV_ORIGINS: [&str; 3] = ["http://localhost:*", "http://127.0.0.1:*", "http://[::1]:*"];

/// The posture id [`OriginPolicy::dangerous_any_origin`] records.
pub const ANY_ORIGIN_OPT_OUT: &str =
  "id_effect_axum::realtime::origin::OriginPolicy::dangerous_any_origin";

/// A port to match: one port, or any (`:*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Port {
  Exact(u16),
  Any,
}

/// One allowlist entry.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
  /// `scheme://host[:port]`, lower-cased, the default port filled in.
  Origin {
    scheme: String,
    host: String,
    port: Port,
  },
  /// The opaque origin `null`.
  Null,
}

impl Entry {
  fn matches(&self, origin: &Entry) -> bool {
    match (self, origin) {
      (Entry::Null, Entry::Null) => true,
      (
        Entry::Origin { scheme, host, port },
        Entry::Origin {
          scheme: s,
          host: h,
          port: Port::Exact(p),
        },
      ) => scheme == s && host == h && (*port == Port::Any || *port == Port::Exact(*p)),
      _ => false,
    }
  }
}

impl fmt::Display for Entry {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Entry::Null => f.write_str("null"),
      Entry::Origin { scheme, host, port } => match port {
        Port::Any => write!(f, "{scheme}://{host}:*"),
        Port::Exact(p) => write!(f, "{scheme}://{host}:{p}"),
      },
    }
  }
}

/// The port a scheme implies when the origin names none.
fn default_port(scheme: &str) -> Option<u16> {
  match scheme {
    "http" | "ws" => Some(80),
    "https" | "wss" => Some(443),
    _ => None,
  }
}

/// Parses an origin (`wildcard`: an allowlist entry, where the port may be `*`).
fn parse(text: &str, wildcard: bool) -> Option<Entry> {
  let text = text.trim();
  if text.eq_ignore_ascii_case("null") {
    return Some(Entry::Null);
  }
  let (scheme, rest) = text.split_once("://")?;
  let scheme = scheme.to_ascii_lowercase();
  let scheme_ok = scheme.starts_with(|c: char| c.is_ascii_lowercase())
    && scheme
      .chars()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-.".contains(c));
  if !scheme_ok || rest.is_empty() {
    return None;
  }
  // `[v6]` or a name; then an optional `:port`.
  let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
    let (addr, after) = v6.split_once(']')?;
    addr.parse::<std::net::Ipv6Addr>().ok()?;
    (format!("[{}]", addr.to_ascii_lowercase()), after)
  } else {
    let end = rest.find(':').unwrap_or(rest.len());
    let host = &rest[..end];
    let host_ok = !host.is_empty()
      && host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if !host_ok {
      return None;
    }
    (host.to_ascii_lowercase(), &rest[end..])
  };
  let port = match port {
    "" => Port::Exact(default_port(&scheme)?),
    ":*" if wildcard => Port::Any,
    p => {
      let digits = p.strip_prefix(':')?;
      if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
      }
      Port::Exact(digits.parse().ok()?)
    }
  };
  Some(Entry::Origin { scheme, host, port })
}

/// An allowlist entry that is not an origin (see [`OriginPolicy::allow`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidOrigin {
  /// The entry as given.
  pub value: String,
}

impl fmt::Display for InvalidOrigin {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "invalid allowed origin {:?}: expected scheme://host[:port], scheme://host:* or null",
      self.value
    )
  }
}

impl std::error::Error for InvalidOrigin {}

/// Why an upgrade's `Origin` was refused; answers **403** Problem Details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginRejection {
  /// The origin is well formed but not on the allowlist.
  NotAllowed(String),
  /// The header is not one origin (not UTF-8, not an origin, or sent twice).
  Malformed,
}

impl fmt::Display for OriginRejection {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      OriginRejection::NotAllowed(origin) => write!(f, "origin {origin} is not allowed"),
      OriginRejection::Malformed => f.write_str("the Origin header is not one origin"),
    }
  }
}

impl std::error::Error for OriginRejection {}

impl IntoResponse for OriginRejection {
  fn into_response(self) -> Response {
    Problem::new(StatusCode::FORBIDDEN)
      .with_title("Origin not allowed")
      .with_detail(self.to_string())
      .into_response()
  }
}

/// The `Origin` allowlist (see the [module docs](self)). The default refuses every browser
/// origin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OriginPolicy {
  entries: Vec<Entry>,
  any: bool,
}

impl OriginPolicy {
  /// Refuses every browser origin (the default); requests without `Origin` still pass.
  pub fn deny_all() -> Self {
    Self::default()
  }

  /// Allows exactly `origins` (`scheme://host[:port]`, `scheme://host:*` or `null`), e.g. from
  /// configuration. An empty list refuses every browser origin.
  ///
  /// # Errors
  ///
  /// [`InvalidOrigin`] for the first entry that is not an origin (a path, a wildcard host, an
  /// unknown scheme without a port).
  pub fn allow<I, T>(origins: I) -> Result<Self, InvalidOrigin>
  where
    I: IntoIterator<Item = T>,
    T: AsRef<str>,
  {
    let entries = origins
      .into_iter()
      .map(|o| {
        parse(o.as_ref(), true).ok_or_else(|| InvalidOrigin {
          value: o.as_ref().to_owned(),
        })
      })
      .collect::<Result<_, _>>()?;
    Ok(Self {
      entries,
      any: false,
    })
  }

  /// Admits every origin: any site the user visits can open a socket with the user's cookies.
  /// Records the opt-out [`ANY_ORIGIN_OPT_OUT`] in the
  /// [posture registry](id_effect::observability::posture).
  pub fn dangerous_any_origin() -> Self {
    id_effect::observability::posture::record_dangerous(
      ANY_ORIGIN_OPT_OUT,
      "R9: WebSocket upgrades from every origin are accepted (cross-site WebSocket hijacking)",
    );
    Self {
      entries: Vec::new(),
      any: true,
    }
  }

  /// `true` after [`Self::dangerous_any_origin`].
  pub fn is_any(&self) -> bool {
    self.any
  }

  /// The allowlist, normalised (`https://a.example:443`, `http://localhost:*`, `null`).
  pub fn entries(&self) -> Vec<String> {
    self.entries.iter().map(ToString::to_string).collect()
  }

  /// Whether the origin `origin` (an `Origin` header value) is allowed.
  pub fn allows(&self, origin: &str) -> bool {
    if self.any {
      return true;
    }
    match parse(origin, false) {
      Some(origin) => self.entries.iter().any(|e| e.matches(&origin)),
      None => false,
    }
  }

  /// Checks the request's `Origin` header (see the [module docs](self)).
  ///
  /// # Errors
  ///
  /// [`OriginRejection`] when a browser origin is not allowed or the header is malformed.
  pub fn check(&self, headers: &HeaderMap) -> Result<(), OriginRejection> {
    let mut values = headers.get_all(header::ORIGIN).iter();
    let Some(value) = values.next() else {
      return Ok(());
    };
    if values.next().is_some() {
      return Err(OriginRejection::Malformed);
    }
    let origin = value.to_str().map_err(|_| OriginRejection::Malformed)?;
    if self.any {
      return Ok(());
    }
    if parse(origin, false).is_none() {
      return Err(OriginRejection::Malformed);
    }
    if self.allows(origin) {
      Ok(())
    } else {
      Err(OriginRejection::NotAllowed(origin.to_owned()))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::http::HeaderValue;

  fn origin(value: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::ORIGIN, HeaderValue::from_str(value).unwrap());
    h
  }

  #[test]
  fn entries_parse_normalise_and_reject() {
    let p = OriginPolicy::allow([
      "HTTPS://App.Example.com",
      "http://localhost:*",
      "http://[::1]:8080",
      "null",
      "custom+x://host:9",
    ])
    .unwrap();
    assert_eq!(
      p.entries(),
      [
        "https://app.example.com:443",
        "http://localhost:*",
        "http://[::1]:8080",
        "null",
        "custom+x://host:9"
      ]
    );
    for bad in [
      "app.example.com",
      "https://",
      "https://a.example/path",
      "https://*.example.com",
      "custom://host",
      "https://a.example:",
      "https://a.example:99999",
      "https://[nope]",
      "1https://a.example",
      "https://a b",
    ] {
      let err = OriginPolicy::allow([bad]).unwrap_err();
      assert_eq!(err.value, bad);
      assert!(err.to_string().contains("invalid allowed origin"));
    }
    assert!(!p.is_any());
  }

  #[test]
  fn matching_fills_default_ports_and_wildcards_only_the_port() {
    let p = OriginPolicy::allow(DEV_ORIGINS.iter().copied().chain(["https://a.example"])).unwrap();
    assert!(p.allows("http://localhost:3000"));
    assert!(p.allows("http://LOCALHOST"));
    assert!(p.allows("http://127.0.0.1:1"));
    assert!(p.allows("http://[::1]:5173"));
    assert!(!p.allows("https://localhost:3000"));
    assert!(!p.allows("http://localhost.evil.example:3000"));
    assert!(p.allows("https://a.example:443"));
    assert!(!p.allows("https://a.example:444"));
    assert!(!p.allows("http://a.example"));
    assert!(!p.allows("null"));
    assert!(
      !p.allows("http://localhost:*"),
      "a wildcard is not an origin"
    );
    assert!(!p.allows("garbage"));
    assert!(OriginPolicy::allow(["null"]).unwrap().allows("null"));
  }

  #[test]
  fn check_reads_the_header() {
    let p = OriginPolicy::allow(["https://a.example"]).unwrap();
    assert_eq!(p.check(&HeaderMap::new()), Ok(()));
    assert_eq!(p.check(&origin("https://a.example")), Ok(()));
    assert_eq!(
      p.check(&origin("https://b.example")),
      Err(OriginRejection::NotAllowed("https://b.example".into()))
    );
    assert_eq!(
      p.check(&origin("not an origin")),
      Err(OriginRejection::Malformed)
    );
    let mut twice = origin("https://a.example");
    twice.append(
      header::ORIGIN,
      HeaderValue::from_static("https://a.example"),
    );
    assert_eq!(p.check(&twice), Err(OriginRejection::Malformed));
    let mut opaque = HeaderMap::new();
    opaque.insert(
      header::ORIGIN,
      HeaderValue::from_bytes(b"https://\xff").unwrap(),
    );
    assert_eq!(p.check(&opaque), Err(OriginRejection::Malformed));
    assert_eq!(
      OriginPolicy::deny_all().check(&origin("https://a.example")),
      Err(OriginRejection::NotAllowed("https://a.example".into()))
    );
    let res = OriginRejection::Malformed.into_response();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(
      OriginRejection::NotAllowed("x".into())
        .to_string()
        .contains("not allowed")
    );
  }

  #[test]
  fn any_origin_is_recorded() {
    let p = OriginPolicy::dangerous_any_origin();
    assert!(p.is_any() && p.allows("https://evil.example") && p.allows("???"));
    assert_eq!(p.check(&origin("https://evil.example")), Ok(()));
    assert!(
      id_effect::observability::posture::dangerous_opt_outs()
        .iter()
        .any(|(id, _)| *id == ANY_ORIGIN_OPT_OUT)
    );
  }
}
