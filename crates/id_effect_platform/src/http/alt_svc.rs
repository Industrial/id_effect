//! The `Alt-Svc` cache of the HTTP/3 client (RFC 7838, feature `http3`).
//!
//! An `https` response carrying `Alt-Svc: h3=":443"; ma=3600` tells the client that the origin
//! also serves HTTP/3 on UDP port 443 for the next hour. [`AltSvcCache`] remembers that per
//! [`Origin`] so the **next** request may go over HTTP/3 ([`Http3Client`](super::Http3Client)).
//!
//! - `ma` (max-age) is honoured: an entry expires `ma` seconds after it was received (24 hours
//!   when absent, at most [`MAX_MAX_AGE`]); `Alt-Svc: clear` removes the origin's entry.
//! - Only the `h3` protocol id is used (draft ids such as `h3-29` are ignored), and only
//!   alternatives on the **same host** (`h3=":443"`, or the origin's own host name): an
//!   alternative naming another host is ignored, so a response cannot point the client's UDP
//!   traffic at a different machine.
//! - The cache is bounded: at most `max_entries` origins, the least recently used evicted
//!   first. Failed origins go to a negative cache (5 minutes by default) and are not tried over
//!   HTTP/3 again until it expires.
//! - [`AltSvcCache::clear`] forgets everything; the client calls it on a network-change error.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Max-age of an alternative without an `ma` parameter (RFC 7838 §3.1: 24 hours).
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// The longest max-age honoured (30 days); a larger `ma` is clamped to it.
pub const MAX_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The protocol id of HTTP/3 in `Alt-Svc` (RFC 9114 §3.1.1).
const H3: &str = "h3";

/// An `https` origin: host (lowercase, without IPv6 brackets or a trailing dot) and port.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
  host: String,
  port: u16,
}

/// `host` lowercased, without IPv6 brackets or a trailing dot.
fn normalize(host: &str) -> String {
  let host = host.strip_prefix('[').unwrap_or(host);
  let host = host.strip_suffix(']').unwrap_or(host);
  host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
}

impl Origin {
  /// The origin `host:port`.
  pub fn new(host: &str, port: u16) -> Self {
    Self {
      host: normalize(host),
      port,
    }
  }

  /// The origin of an `https` URL (port 443 when absent); `None` for any other scheme or a URL
  /// without a host. HTTP/3 is only used for `https`.
  pub fn from_url(url: &::reqwest::Url) -> Option<Self> {
    if url.scheme() != "https" {
      return None;
    }
    let host = url.host_str()?;
    Some(Self::new(host, url.port_or_known_default()?))
  }

  /// The host, normalized.
  pub fn host(&self) -> &str {
    &self.host
  }

  /// The port.
  pub fn port(&self) -> u16 {
    self.port
  }
}

/// One HTTP/3 alternative of an `Alt-Svc` header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alternative {
  /// UDP port of the alternative.
  pub port: u16,
  /// How long the alternative may be used ([`DEFAULT_MAX_AGE`] when the header has no `ma`).
  pub max_age: Duration,
}

/// A parsed `Alt-Svc` header value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AltSvc {
  /// `clear`: forget the origin's alternatives.
  Clear,
  /// The usable HTTP/3 alternatives, in the server's order of preference (possibly none).
  Alternatives(Vec<Alternative>),
}

/// Splits `s` on `sep` outside double-quoted strings.
fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
  let mut parts = Vec::new();
  let mut quoted = false;
  let mut start = 0;
  for (i, c) in s.char_indices() {
    if c == '"' {
      quoted = !quoted;
    } else if c == sep && !quoted {
      parts.push(&s[start..i]);
      start = i + 1;
    }
  }
  parts.push(&s[start..]);
  parts
}

/// The port of an alt-authority (`":443"` or `"host:443"`) when its host is empty or
/// `origin_host`.
fn same_host_port(authority: &str, origin_host: &str) -> Option<u16> {
  let authority = authority.strip_prefix('"')?.strip_suffix('"')?;
  let (host, port) = authority.rsplit_once(':')?;
  if !host.is_empty() && normalize(host) != normalize(origin_host) {
    return None;
  }
  port.parse::<u16>().ok().filter(|p| *p != 0)
}

/// The max-age of an alternative's parameters (`ma=<seconds>`; [`DEFAULT_MAX_AGE`] when absent
/// or unparsable, clamped to [`MAX_MAX_AGE`]).
fn max_age<'a>(params: impl Iterator<Item = &'a str>) -> Duration {
  params
    .filter_map(|p| p.trim().split_once('='))
    .find(|(k, _)| k.trim().eq_ignore_ascii_case("ma"))
    .and_then(|(_, v)| v.trim().trim_matches('"').parse::<u64>().ok())
    .map_or(DEFAULT_MAX_AGE, |secs| {
      Duration::from_secs(secs).min(MAX_MAX_AGE)
    })
}

/// Parses an `Alt-Svc` header value received from an origin on `origin_host`, keeping only
/// same-host `h3` alternatives.
pub fn parse(value: &str, origin_host: &str) -> AltSvc {
  if value.trim().eq_ignore_ascii_case("clear") {
    return AltSvc::Clear;
  }
  let alternatives = split_unquoted(value, ',')
    .into_iter()
    .filter_map(|alt| {
      let mut params = split_unquoted(alt, ';').into_iter();
      let (protocol, authority) = params.next()?.trim().split_once('=')?;
      if protocol.trim() != H3 {
        return None;
      }
      let port = same_host_port(authority.trim(), origin_host)?;
      Some(Alternative {
        port,
        max_age: max_age(params),
      })
    })
    .collect();
  AltSvc::Alternatives(alternatives)
}

/// A cached alternative.
#[derive(Clone, Copy, Debug)]
struct Entry {
  port: u16,
  expires: Instant,
  /// Recency stamp for LRU eviction.
  used: u64,
}

#[derive(Debug, Default)]
struct State {
  entries: HashMap<Origin, Entry>,
  /// Origins whose HTTP/3 attempt failed, with the end of their exclusion.
  negative: HashMap<Origin, Instant>,
  tick: u64,
}

impl State {
  fn stamp(&mut self) -> u64 {
    self.tick += 1;
    self.tick
  }
}

/// `now + age`, or the far future when that overflows.
fn after(now: Instant, age: Duration) -> Instant {
  now.checked_add(age).unwrap_or(now + MAX_MAX_AGE)
}

/// The bounded, thread-safe `Alt-Svc` cache (see the [module docs](self)).
#[derive(Debug)]
pub struct AltSvcCache {
  state: Mutex<State>,
  max_entries: usize,
  negative_ttl: Duration,
}

impl AltSvcCache {
  /// A cache holding at most `max_entries` origins (at least 1), excluding a failed origin for
  /// `negative_ttl`.
  pub fn new(max_entries: usize, negative_ttl: Duration) -> Self {
    Self {
      state: Mutex::new(State::default()),
      max_entries: max_entries.max(1),
      negative_ttl,
    }
  }

  fn state(&self) -> std::sync::MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Applies an `Alt-Svc` header `value` received from `origin` at `now`: `clear` removes the
  /// origin's entry, an `h3` alternative replaces it (the first one, the server's preference).
  pub fn record(&self, origin: &Origin, value: &str, now: Instant) {
    match parse(value, origin.host()) {
      AltSvc::Clear => {
        self.state().entries.remove(origin);
      }
      AltSvc::Alternatives(alts) => {
        if let Some(alt) = alts.first() {
          self.insert(origin, alt.port, alt.max_age, now);
        }
      }
    }
  }

  /// Remembers that `origin` serves HTTP/3 on UDP `port` for `max_age` from `now` (clamped to
  /// [`MAX_MAX_AGE`]), evicting the least recently used origin when the cache is full.
  pub fn insert(&self, origin: &Origin, port: u16, max_age: Duration, now: Instant) {
    let mut state = self.state();
    if !state.entries.contains_key(origin) && state.entries.len() >= self.max_entries {
      let oldest = state
        .entries
        .iter()
        .min_by_key(|(_, e)| e.used)
        .map(|(o, _)| o.clone());
      if let Some(oldest) = oldest {
        state.entries.remove(&oldest);
      }
    }
    let used = state.stamp();
    state.entries.insert(
      origin.clone(),
      Entry {
        port,
        expires: after(now, max_age.min(MAX_MAX_AGE)),
        used,
      },
    );
  }

  /// The UDP port to try HTTP/3 on for `origin` at `now`: `None` when nothing is cached, the
  /// entry expired (it is removed) or the origin is in the negative cache.
  pub fn lookup(&self, origin: &Origin, now: Instant) -> Option<u16> {
    if self.is_negative(origin, now) {
      return None;
    }
    let mut state = self.state();
    let entry = *state.entries.get(origin)?;
    if entry.expires <= now {
      state.entries.remove(origin);
      return None;
    }
    let used = state.stamp();
    if let Some(e) = state.entries.get_mut(origin) {
      e.used = used;
    }
    Some(entry.port)
  }

  /// Puts `origin` in the negative cache from `now`: it is not tried over HTTP/3 for the
  /// negative TTL. The negative cache is bounded like the entries (soonest expiry evicted).
  pub fn mark_failed(&self, origin: &Origin, now: Instant) {
    let mut state = self.state();
    if !state.negative.contains_key(origin) && state.negative.len() >= self.max_entries {
      let soonest = state
        .negative
        .iter()
        .min_by_key(|(_, until)| **until)
        .map(|(o, _)| o.clone());
      if let Some(soonest) = soonest {
        state.negative.remove(&soonest);
      }
    }
    state
      .negative
      .insert(origin.clone(), after(now, self.negative_ttl));
  }

  /// Whether `origin` is in the negative cache at `now` (an expired exclusion is removed).
  pub fn is_negative(&self, origin: &Origin, now: Instant) -> bool {
    let mut state = self.state();
    match state.negative.get(origin) {
      Some(until) if *until > now => true,
      Some(_) => {
        state.negative.remove(origin);
        false
      }
      None => false,
    }
  }

  /// Forgets every alternative and every exclusion (a network change: the new network may
  /// reach UDP where the old one did not, and the old alternatives may not apply).
  pub fn clear(&self) {
    let mut state = self.state();
    state.entries.clear();
    state.negative.clear();
  }

  /// Number of cached origins (expired entries included until looked up).
  pub fn len(&self) -> usize {
    self.state().entries.len()
  }

  /// Whether no origin is cached.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const HOUR: Duration = Duration::from_secs(3600);

  fn alt(port: u16, max_age: Duration) -> Alternative {
    Alternative { port, max_age }
  }

  mod parse {
    use super::*;

    #[test]
    fn same_host_h3_alternatives_with_max_age() {
      assert_eq!(
        parse(
          r#"h3=":443"; ma=3600, h3="Example.COM.:8443""#,
          "example.com"
        ),
        AltSvc::Alternatives(vec![alt(443, HOUR), alt(8443, DEFAULT_MAX_AGE)])
      );
    }

    #[test]
    fn clear_and_ignored_alternatives() {
      assert_eq!(parse(" Clear ", "a.test"), AltSvc::Clear);
      // Other hosts, draft ids, other protocols, bad ports and malformed values are dropped.
      let value = r#"h3="evil.test:443", h3-29=":443", h2=":443", h3=":0", h3=":x", h3=443, junk, h3=":70000""#;
      assert_eq!(parse(value, "a.test"), AltSvc::Alternatives(vec![]));
    }

    #[test]
    fn ipv6_hosts_and_parameters() {
      assert_eq!(
        parse(r#"h3="[::1]:4433"; persist=1; MA="7""#, "[::1]"),
        AltSvc::Alternatives(vec![alt(4433, Duration::from_secs(7))])
      );
      // Unparsable ma falls back to the default; a huge one is clamped.
      assert_eq!(
        parse(r#"h3=":1"; ma=soon, h3=":2"; ma=99999999999"#, "a"),
        AltSvc::Alternatives(vec![alt(1, DEFAULT_MAX_AGE), alt(2, MAX_MAX_AGE)])
      );
    }

    #[test]
    fn commas_inside_quotes_do_not_split() {
      assert_eq!(parse(r#"h3=",:443""#, "a"), AltSvc::Alternatives(vec![]));
    }
  }

  mod origin {
    use super::*;

    #[test]
    fn from_url_only_for_https() {
      let url = ::reqwest::Url::parse("https://EXAMPLE.com/x").unwrap();
      let origin = Origin::from_url(&url).unwrap();
      assert_eq!((origin.host(), origin.port()), ("example.com", 443));
      let url = ::reqwest::Url::parse("https://[::1]:8443/").unwrap();
      assert_eq!(Origin::from_url(&url), Some(Origin::new("::1", 8443)));
      let url = ::reqwest::Url::parse("http://example.com/").unwrap();
      assert_eq!(Origin::from_url(&url), None);
    }
  }

  mod cache {
    use super::*;

    fn origin(n: u16) -> Origin {
      Origin::new("a.test", n)
    }

    #[test]
    fn record_honours_max_age_and_clear() {
      let cache = AltSvcCache::new(8, Duration::from_secs(300));
      let now = Instant::now();
      assert!(cache.is_empty());
      cache.record(&origin(1), r#"h3=":4433"; ma=60"#, now);
      assert_eq!(cache.lookup(&origin(1), now), Some(4433));
      assert_eq!(
        cache.lookup(&origin(1), now + Duration::from_secs(59)),
        Some(4433)
      );
      assert_eq!(
        cache.lookup(&origin(1), now + Duration::from_secs(60)),
        None
      );
      assert!(cache.is_empty(), "the expired entry is removed");
      cache.record(&origin(1), r#"h3=":4433""#, now);
      cache.record(&origin(1), r#"h2=":443""#, now);
      assert_eq!(cache.lookup(&origin(1), now), Some(4433), "no h3: kept");
      cache.record(&origin(1), "clear", now);
      assert_eq!(cache.lookup(&origin(1), now), None);
    }

    #[test]
    fn bounded_with_lru_eviction() {
      let cache = AltSvcCache::new(2, Duration::from_secs(300));
      let now = Instant::now();
      cache.insert(&origin(1), 1, HOUR, now);
      cache.insert(&origin(2), 2, HOUR, now);
      // Touch 1, so 2 is the least recently used.
      assert_eq!(cache.lookup(&origin(1), now), Some(1));
      cache.insert(&origin(3), 3, HOUR, now);
      assert_eq!(cache.len(), 2);
      assert_eq!(cache.lookup(&origin(2), now), None);
      assert_eq!(cache.lookup(&origin(1), now), Some(1));
      assert_eq!(cache.lookup(&origin(3), now), Some(3));
      // Replacing an entry does not evict.
      cache.insert(&origin(3), 30, HOUR, now);
      assert_eq!(cache.len(), 2);
      assert_eq!(cache.lookup(&origin(3), now), Some(30));
      // A zero bound still holds one.
      let tiny = AltSvcCache::new(0, Duration::from_secs(1));
      tiny.insert(&origin(1), 1, HOUR, now);
      tiny.insert(&origin(2), 2, HOUR, now);
      assert_eq!(tiny.len(), 1);
    }

    #[test]
    fn negative_cache_expires_and_is_bounded() {
      let ttl = Duration::from_secs(300);
      let cache = AltSvcCache::new(2, ttl);
      let now = Instant::now();
      cache.insert(&origin(1), 1, HOUR, now);
      cache.mark_failed(&origin(1), now);
      assert!(cache.is_negative(&origin(1), now));
      assert_eq!(cache.lookup(&origin(1), now + ttl / 2), None);
      assert!(!cache.is_negative(&origin(1), now + ttl));
      assert_eq!(cache.lookup(&origin(1), now + ttl), Some(1));
      cache.mark_failed(&origin(1), now);
      cache.mark_failed(&origin(2), now + Duration::from_secs(1));
      cache.mark_failed(&origin(3), now + Duration::from_secs(2));
      assert!(!cache.is_negative(&origin(1), now), "soonest evicted");
      assert!(cache.is_negative(&origin(2), now));
      assert!(cache.is_negative(&origin(3), now));
      cache.mark_failed(&origin(3), now);
    }

    #[test]
    fn clear_forgets_entries_and_exclusions() {
      let cache = AltSvcCache::new(4, Duration::from_secs(300));
      let now = Instant::now();
      cache.insert(&origin(1), 1, HOUR, now);
      cache.mark_failed(&origin(2), now);
      cache.clear();
      assert!(cache.is_empty());
      assert!(!cache.is_negative(&origin(2), now));
    }

    #[test]
    fn far_future_ages_do_not_overflow() {
      let now = Instant::now();
      assert!(after(now, Duration::MAX) > now);
      let cache = AltSvcCache::new(1, Duration::MAX);
      cache.mark_failed(&origin(1), now);
      assert!(cache.is_negative(&origin(1), now));
    }
  }
}
