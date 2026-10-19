//! The outbound request guard (M3, ADR 0013 R6): an [`SsrfPolicy`] the reqwest client enforces
//! before it connects anywhere.
//!
//! [`HttpClientConfig::default`](super::HttpClientConfig::default) turns it on. Before a request
//! is sent the client checks its URL: the scheme is `http` or `https`, and a literal-IP host is
//! not in a forbidden range. A host name goes through the client's DNS resolver, which checks
//! **every** address the name resolves to and refuses the name if any of them is forbidden; the
//! addresses checked are the ones the connection uses, so DNS rebinding cannot swap in a private
//! address after the check. Forbidden ([`forbidden_range`]): loopback, unspecified, RFC 1918,
//! carrier-grade NAT `100.64.0.0/10`, link-local `169.254.0.0/16` and `fe80::/10` (cloud metadata
//! `169.254.169.254`, `fd00:ec2::254`), unique local `fc00::/7`, multicast, reserved and
//! broadcast, and every IPv4-mapped / IPv4-compatible IPv6 form (NAT64 and 6to4 forms of a
//! forbidden IPv4 address too).
//!
//! Exemptions come from config: [`SsrfPolicy::allow_hosts`] (a host name or literal IP, matched
//! exactly and case-insensitively; a listed name's addresses are not checked) and
//! [`SsrfPolicy::allow_cidrs`] (addresses inside the ranges pass). The policy deserializes from
//! a config section:
//!
//! ```toml
//! allow_hosts = ["sendgrid.internal", "127.0.0.1"]
//! allow_cidrs = ["10.20.0.0/16"]
//! ```
//!
//! [`SsrfPolicy::dangerous_allow_private_networks`] turns the range check off (schemes are still
//! checked) and records the opt-out in the posture registry
//! ([`id_effect::observability::posture`]).
//!
//! Redirects are not followed unless
//! [`HttpClientConfig::follow_redirects`](super::HttpClientConfig::follow_redirects) allows them,
//! and then every hop is checked like the first request. Behind an HTTP proxy
//! (`HTTP_PROXY` / `HTTPS_PROXY`) the proxy resolves host names, so the guard checks literal-IP
//! URLs and the proxy's own address only.

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

use ::reqwest::Url;
use ::reqwest::dns::{Addrs, Name, Resolve, Resolving};
use id_effect::observability::posture::record_dangerous;
use serde::Deserialize;

/// Posture id of [`SsrfPolicy::dangerous_allow_private_networks`].
pub const DANGEROUS_ALLOW_PRIVATE_NETWORKS: &str =
  "id_effect_platform::http::SsrfPolicy::dangerous_allow_private_networks";

const REOPENS_PRIVATE_NETWORKS: &str = "M3: outbound HTTP may reach loopback, private, link-local \
  and cloud metadata addresses (SSRF)";

/// An IP address range in CIDR notation (`10.0.0.0/8`, `fd00::/8`; a bare address is a `/32` or
/// `/128`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct IpCidr {
  addr: IpAddr,
  prefix: u8,
}

impl IpCidr {
  /// The range of `prefix` leading bits of `addr` (`None` if `prefix` is longer than the
  /// address).
  pub fn new(addr: IpAddr, prefix: u8) -> Option<Self> {
    let bits = if addr.is_ipv4() { 32 } else { 128 };
    (prefix <= bits).then_some(Self { addr, prefix })
  }

  /// Whether `ip` is inside the range (an IPv4 range never contains an IPv6 address, mapped
  /// forms included).
  pub fn contains(&self, ip: IpAddr) -> bool {
    match (self.addr, ip) {
      (IpAddr::V4(net), IpAddr::V4(ip)) => {
        let mask = u32::MAX
          .checked_shl(32 - u32::from(self.prefix))
          .unwrap_or(0);
        u32::from(net) & mask == u32::from(ip) & mask
      }
      (IpAddr::V6(net), IpAddr::V6(ip)) => {
        let mask = u128::MAX
          .checked_shl(128 - u32::from(self.prefix))
          .unwrap_or(0);
        u128::from(net) & mask == u128::from(ip) & mask
      }
      _ => false,
    }
  }
}

impl fmt::Display for IpCidr {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}/{}", self.addr, self.prefix)
  }
}

impl FromStr for IpCidr {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let bad = || format!("invalid CIDR {s:?}: expected `<ip>/<prefix>` or `<ip>`");
    let (addr, prefix) = match s.split_once('/') {
      Some((addr, prefix)) => (addr, Some(prefix)),
      None => (s, None),
    };
    let addr: IpAddr = addr.trim().parse().map_err(|_| bad())?;
    let prefix = match prefix {
      Some(p) => p.trim().parse().map_err(|_| bad())?,
      None if addr.is_ipv4() => 32,
      None => 128,
    };
    Self::new(addr, prefix).ok_or_else(bad)
  }
}

impl TryFrom<String> for IpCidr {
  type Error = String;

  fn try_from(s: String) -> Result<Self, Self::Error> {
    s.parse()
  }
}

/// The forbidden range `ip` is in, named (`"127.0.0.0/8 (loopback)"`), or `None` for an address
/// the guard lets through.
pub fn forbidden_range(ip: IpAddr) -> Option<&'static str> {
  match ip {
    IpAddr::V4(v4) => v4_range(v4),
    IpAddr::V6(v6) => v6_range(v6),
  }
}

fn v4_range(ip: Ipv4Addr) -> Option<&'static str> {
  let [a, b, c, _] = ip.octets();
  Some(match (a, b, c) {
    (0, ..) => "0.0.0.0/8 (unspecified, this network)",
    (10, ..) => "10.0.0.0/8 (private, RFC 1918)",
    (100, 64..=127, _) => "100.64.0.0/10 (carrier-grade NAT)",
    (127, ..) => "127.0.0.0/8 (loopback)",
    (169, 254, _) => "169.254.0.0/16 (link-local, cloud metadata)",
    (172, 16..=31, _) => "172.16.0.0/12 (private, RFC 1918)",
    (192, 0, 0) => "192.0.0.0/24 (IETF protocol assignments)",
    (192, 168, _) => "192.168.0.0/16 (private, RFC 1918)",
    (198, 18..=19, _) => "198.18.0.0/15 (benchmarking)",
    (224..=239, ..) => "224.0.0.0/4 (multicast)",
    (240..=255, ..) => "240.0.0.0/4 (reserved, broadcast)",
    _ => return None,
  })
}

fn v6_range(ip: Ipv6Addr) -> Option<&'static str> {
  let s = ip.segments();
  let embedded = |hi: u16, lo: u16| {
    let [a, b] = hi.to_be_bytes();
    let [c, d] = lo.to_be_bytes();
    v4_range(Ipv4Addr::new(a, b, c, d))
  };
  Some(match s {
    [0, 0, 0, 0, 0, 0, 0, 0] => "::/128 (unspecified)",
    [0, 0, 0, 0, 0, 0, 0, 1] => "::1/128 (loopback)",
    [0, 0, 0, 0, 0, 0xffff, ..] => "::ffff:0:0/96 (IPv4-mapped)",
    [0, 0, 0, 0, 0, 0, ..] => "::/96 (IPv4-compatible)",
    [0x64, 0xff9b, 0, 0, 0, 0, hi, lo] if embedded(hi, lo).is_some() => {
      "64:ff9b::/96 (NAT64 form of a forbidden IPv4 address)"
    }
    [0x2002, hi, lo, ..] if embedded(hi, lo).is_some() => {
      "2002::/16 (6to4 form of a forbidden IPv4 address)"
    }
    [first, ..] if first & 0xffc0 == 0xfe80 => "fe80::/10 (link-local)",
    [first, ..] if first & 0xffc0 == 0xfec0 => "fec0::/10 (site-local)",
    [first, ..] if first & 0xfe00 == 0xfc00 => "fc00::/7 (unique local, cloud metadata)",
    [first, ..] if first & 0xff00 == 0xff00 => "ff00::/8 (multicast)",
    _ => return None,
  })
}

/// A request the [`SsrfPolicy`] refused: a scheme other than `http`/`https`, or a destination
/// address in a forbidden range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SsrfBlocked {
  reason: String,
}

impl SsrfBlocked {
  fn new(reason: String) -> Self {
    Self { reason }
  }

  /// Why the request was refused.
  pub fn reason(&self) -> &str {
    &self.reason
  }
}

impl fmt::Display for SsrfBlocked {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "blocked by the SSRF policy: {}", self.reason)
  }
}

impl std::error::Error for SsrfBlocked {}

/// Which destinations outbound requests may reach (the module docs list the forbidden ranges).
///
/// [`SsrfPolicy::default`] refuses every forbidden range with no exemption.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SsrfPolicy {
  /// Hosts exempt from the range check: host names or literal IPs, matched exactly and
  /// case-insensitively (no wildcards). A listed name's resolved addresses are not checked.
  pub allow_hosts: Vec<String>,
  /// Address ranges exempt from the range check (literal-IP URLs and resolved addresses).
  pub allow_cidrs: Vec<IpCidr>,
  /// Set only by [`SsrfPolicy::dangerous_allow_private_networks`], never from config.
  #[serde(skip)]
  private_networks: bool,
}

/// `host` as compared with [`SsrfPolicy::allow_hosts`]: no IPv6 brackets, no trailing dot.
fn normalize_host(host: &str) -> &str {
  let host = host.strip_prefix('[').unwrap_or(host);
  let host = host.strip_suffix(']').unwrap_or(host);
  host.strip_suffix('.').unwrap_or(host)
}

impl SsrfPolicy {
  /// The default policy: every forbidden range refused, no exemption.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Adds `hosts` to [`allow_hosts`](Self::allow_hosts).
  pub fn allow_hosts<I, S>(mut self, hosts: I) -> Self
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    self.allow_hosts.extend(hosts.into_iter().map(Into::into));
    self
  }

  /// Adds `cidrs` to [`allow_cidrs`](Self::allow_cidrs).
  pub fn allow_cidrs(mut self, cidrs: impl IntoIterator<Item = IpCidr>) -> Self {
    self.allow_cidrs.extend(cidrs);
    self
  }

  /// Turns the range check off: requests may reach loopback, private, link-local and metadata
  /// addresses (schemes are still checked). Records [`DANGEROUS_ALLOW_PRIVATE_NETWORKS`] in the
  /// posture registry, which the prod posture check refuses unless the risk is accepted.
  pub fn dangerous_allow_private_networks(mut self) -> Self {
    record_dangerous(DANGEROUS_ALLOW_PRIVATE_NETWORKS, REOPENS_PRIVATE_NETWORKS);
    self.private_networks = true;
    self
  }

  /// Whether [`dangerous_allow_private_networks`](Self::dangerous_allow_private_networks) turned
  /// the range check off.
  #[inline]
  pub fn allows_private_networks(&self) -> bool {
    self.private_networks
  }

  fn host_allowed(&self, host: &str) -> bool {
    let host = normalize_host(host);
    self
      .allow_hosts
      .iter()
      .any(|h| normalize_host(h).eq_ignore_ascii_case(host))
  }

  fn check_ip(&self, host: &str, ip: IpAddr) -> Result<(), SsrfBlocked> {
    if self.allow_cidrs.iter().any(|c| c.contains(ip)) {
      return Ok(());
    }
    match forbidden_range(ip) {
      Some(range) => Err(SsrfBlocked::new(format!(
        "{host} ({ip}) is in {range}; allow it with allow_hosts or allow_cidrs"
      ))),
      None => Ok(()),
    }
  }

  /// Checks `url` before it is requested: the scheme, and a literal-IP host's range (a host
  /// name is checked when it resolves, [`check_resolved`](Self::check_resolved)).
  pub fn check_url(&self, url: &str) -> Result<(), SsrfBlocked> {
    let parsed =
      Url::parse(url).map_err(|e| SsrfBlocked::new(format!("{url:?} is not a URL: {e}")))?;
    self.check_parsed(&parsed)
  }

  pub(super) fn check_parsed(&self, url: &Url) -> Result<(), SsrfBlocked> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
      return Err(SsrfBlocked::new(format!(
        "scheme `{scheme}` is not http or https"
      )));
    }
    let host = url.host_str().unwrap_or_default();
    if self.private_networks || self.host_allowed(host) {
      return Ok(());
    }
    match normalize_host(host).parse::<IpAddr>() {
      Ok(ip) => self.check_ip(host, ip),
      Err(_) => Ok(()),
    }
  }

  /// Checks the addresses `host` resolved to: refused if **any** of them is in a forbidden range
  /// (unless `host` is in [`allow_hosts`](Self::allow_hosts) or the address in
  /// [`allow_cidrs`](Self::allow_cidrs)). The client's DNS resolver calls this on every lookup.
  pub fn check_resolved(
    &self,
    host: &str,
    addrs: impl IntoIterator<Item = IpAddr>,
  ) -> Result<(), SsrfBlocked> {
    if self.private_networks || self.host_allowed(host) {
      return Ok(());
    }
    addrs.into_iter().try_for_each(|ip| self.check_ip(host, ip))
  }
}

/// A DNS lookup: the addresses of a host name.
type Lookup = Arc<
  dyn Fn(String) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>> + Send + Sync,
>;

/// The client's DNS resolver: the system lookup, then [`SsrfPolicy::check_resolved`] over every
/// address before reqwest may connect to one.
pub(super) struct GuardedResolver {
  policy: Arc<SsrfPolicy>,
  lookup: Lookup,
}

impl GuardedResolver {
  pub(super) fn new(policy: Arc<SsrfPolicy>) -> Self {
    Self::with_lookup(
      policy,
      Arc::new(|host| {
        Box::pin(async move { Ok(tokio::net::lookup_host((host.as_str(), 0)).await?.collect()) })
      }),
    )
  }

  fn with_lookup(policy: Arc<SsrfPolicy>, lookup: Lookup) -> Self {
    Self { policy, lookup }
  }
}

impl Resolve for GuardedResolver {
  fn resolve(&self, name: Name) -> Resolving {
    let policy = Arc::clone(&self.policy);
    let host = name.as_str().to_owned();
    let lookup = (self.lookup)(host.clone());
    Box::pin(async move {
      let addrs = lookup.await?;
      policy.check_resolved(&host, addrs.iter().map(SocketAddr::ip))?;
      Ok(Box::new(addrs.into_iter()) as Addrs)
    })
  }
}

/// The redirect policy: none when `max` is 0, else up to `max` hops, each checked against
/// `policy` ([`SsrfPolicy::check_parsed`]; a host name is checked again when it resolves).
pub(super) fn redirect_policy(policy: Arc<SsrfPolicy>, max: usize) -> ::reqwest::redirect::Policy {
  if max == 0 {
    return ::reqwest::redirect::Policy::none();
  }
  ::reqwest::redirect::Policy::custom(move |attempt| {
    if attempt.previous().len() > max {
      let too_many = format!("too many redirects (max {max})");
      return attempt.error(too_many);
    }
    match policy.check_parsed(attempt.url()) {
      Ok(()) => attempt.follow(),
      Err(blocked) => attempt.error(blocked),
    }
  })
}

/// The [`SsrfBlocked`] behind a failed send (the resolver's or a redirect hop's), if any.
pub(super) fn blocked_by(err: &::reqwest::Error) -> Option<SsrfBlocked> {
  let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(err);
  while let Some(e) = cause {
    if let Some(blocked) = e.downcast_ref::<SsrfBlocked>() {
      return Some(blocked.clone());
    }
    cause = e.source();
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;

  fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
  }

  #[test]
  fn cidr_parse_contains_and_display() {
    let net: IpCidr = "10.20.0.0/16".parse().unwrap();
    assert!(net.contains(ip("10.20.3.4")));
    assert!(!net.contains(ip("10.21.0.1")));
    assert!(!net.contains(ip("::ffff:10.20.3.4")));
    assert_eq!(net.to_string(), "10.20.0.0/16");
    let one: IpCidr = "fd00:ec2::254".parse().unwrap();
    assert!(one.contains(ip("fd00:ec2::254")));
    assert!(!one.contains(ip("fd00:ec2::255")));
    let all: IpCidr = "0.0.0.0/0".parse().unwrap();
    assert!(all.contains(ip("8.8.8.8")));
    let all6: IpCidr = "::/0".parse().unwrap();
    assert!(all6.contains(ip("2001:db8::1")));
    for bad in ["10.0.0.0/33", "::/129", "nope", "10.0.0.0/x"] {
      assert!(bad.parse::<IpCidr>().is_err(), "{bad}");
    }
    assert!(IpCidr::try_from("10.0.0.0/8".to_owned()).is_ok());
  }

  #[test]
  fn public_addresses_pass_and_forms_of_forbidden_ones_do_not() {
    for public in [
      "8.8.8.8",
      "1.1.1.1",
      "2606:4700::1111",
      "64:ff9b::808:808",
      "2002:808:808::",
    ] {
      assert_eq!(forbidden_range(ip(public)), None, "{public}");
    }
    for forbidden in [
      "64:ff9b::7f00:1",
      "2002:a00:1::",
      "fec0::1",
      "ff02::1",
      "::1.2.3.4",
      "198.18.0.1",
      "192.0.0.170",
      "255.255.255.255",
    ] {
      assert!(forbidden_range(ip(forbidden)).is_some(), "{forbidden}");
    }
  }

  #[test]
  fn scheme_url_and_dangerous_opt_out() {
    let policy = SsrfPolicy::new();
    let err = policy.check_url("file:///etc/passwd").unwrap_err();
    assert!(err.reason().contains("scheme `file`"), "{err}");
    assert!(policy.check_url("not a url").is_err());
    assert!(policy.check_url("https://example.com/").is_ok());
    let open = SsrfPolicy::new().dangerous_allow_private_networks();
    assert!(open.allows_private_networks());
    assert!(open.check_url("http://127.0.0.1/").is_ok());
    assert!(open.check_resolved("db", [ip("10.0.0.5")]).is_ok());
    assert!(open.check_url("gopher://127.0.0.1/").is_err());
    let recorded = id_effect::observability::posture::dangerous_opt_outs();
    assert!(
      recorded
        .iter()
        .any(|(id, _)| *id == DANGEROUS_ALLOW_PRIVATE_NETWORKS)
    );
  }

  #[test]
  fn deserializes_from_config_and_refuses_the_opt_out_there() {
    let policy: SsrfPolicy = serde_json::from_value(serde_json::json!({
      "allow_hosts": ["mail.internal"],
      "allow_cidrs": ["10.20.0.0/16"],
    }))
    .unwrap();
    assert_eq!(policy.allow_hosts, ["mail.internal"]);
    assert!(policy.check_url("http://10.20.1.1/").is_ok());
    assert!(policy.check_url("http://10.21.1.1/").is_err());
    assert!(!policy.allows_private_networks());
    let opt_out = serde_json::from_value::<SsrfPolicy>(serde_json::json!({
      "private_networks": true,
    }));
    assert!(opt_out.is_err());
    let bad = serde_json::from_value::<SsrfPolicy>(serde_json::json!({ "allow_cidrs": ["x"] }));
    assert!(bad.is_err());
  }

  /// A resolver whose lookup answers `addrs` for every name.
  fn fake(policy: SsrfPolicy, addrs: &[&str]) -> GuardedResolver {
    let addrs: Vec<SocketAddr> = addrs.iter().map(|a| SocketAddr::new(ip(a), 0)).collect();
    GuardedResolver::with_lookup(
      Arc::new(policy),
      Arc::new(move |_| {
        let addrs = addrs.clone();
        Box::pin(async move { Ok(addrs) })
      }),
    )
  }

  async fn resolve(resolver: &GuardedResolver, host: &str) -> Result<Vec<SocketAddr>, String> {
    match resolver.resolve(host.parse().unwrap()).await {
      Ok(addrs) => Ok(addrs.collect()),
      Err(e) => Err(e.to_string()),
    }
  }

  #[tokio::test]
  async fn resolver_refuses_a_name_with_any_forbidden_address() {
    let mixed = fake(SsrfPolicy::new(), &["93.184.215.14", "169.254.169.254"]);
    let err = resolve(&mixed, "rebind.example").await.unwrap_err();
    assert!(err.contains("169.254.169.254"), "{err}");
    let public = fake(SsrfPolicy::new(), &["93.184.215.14", "2606:4700::1111"]);
    assert_eq!(resolve(&public, "example.com").await.unwrap().len(), 2);
    let allowed = fake(
      SsrfPolicy::new().allow_hosts(["Rebind.Example."]),
      &["10.0.0.1"],
    );
    assert_eq!(resolve(&allowed, "rebind.example").await.unwrap().len(), 1);
  }

  #[tokio::test]
  async fn system_resolver_checks_localhost() {
    let resolver = GuardedResolver::new(Arc::new(SsrfPolicy::new()));
    let err = resolve(&resolver, "localhost").await.unwrap_err();
    assert!(err.contains("loopback"), "{err}");
  }
}
