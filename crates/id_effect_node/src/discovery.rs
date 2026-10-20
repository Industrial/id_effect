//! Peer discovery strategies (libcluster's shape).
//!
//! Discovery answers one question: *which addresses should I announce to
//! when joining?* Three strategies per ADR 0009: a static list, DNS
//! resolution (Kubernetes headless services), and “gossip seed” — any
//! member already known from a previous epoch.
//!
//! With feature `kubernetes`, `KubernetesDiscovery` asks the API server
//! itself for the ready endpoints of a headless service (EndpointSlices over
//! rustls, service-account token and CA; no kube-rs, D18).

use async_trait::async_trait;

#[cfg(feature = "kubernetes")]
pub use kubernetes::{KubernetesDiscovery, KubernetesError, SERVICE_ACCOUNT_DIR};

/// Strategy for finding join candidates.
// async-trait emits `#[must_use]` on methods returning an already-must_use boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Discovery: Send + Sync {
  /// Addresses to announce to, in preference order. An empty result
  /// means “I am the first node” — the caller forms a new cluster.
  async fn candidates(&self) -> Vec<String>;
}

/// Fixed peer list (config-file clusters, tests).
pub struct StaticDiscovery {
  peers: Vec<String>,
}

impl StaticDiscovery {
  /// Discovery from a fixed address list.
  pub fn new(peers: impl IntoIterator<Item = impl Into<String>>) -> Self {
    Self {
      peers: peers.into_iter().map(Into::into).collect(),
    }
  }
}

#[async_trait]
impl Discovery for StaticDiscovery {
  async fn candidates(&self) -> Vec<String> {
    self.peers.clone()
  }
}

/// DNS-based discovery: resolve one hostname to many peers (Kubernetes
/// headless services, SRV-less round-robin DNS).
pub struct DnsDiscovery {
  /// `host:port` — the host part is resolved, the port is applied to
  /// every resolved address.
  query: String,
}

impl DnsDiscovery {
  /// Discovery resolving `host:port`.
  pub fn new(query: impl Into<String>) -> Self {
    Self {
      query: query.into(),
    }
  }
}

#[async_trait]
impl Discovery for DnsDiscovery {
  async fn candidates(&self) -> Vec<String> {
    match tokio::net::lookup_host(&self.query).await {
      Ok(addrs) => addrs.map(|a| a.to_string()).collect(),
      Err(_) => Vec::new(),
    }
  }
}

/// Chain multiple strategies: first non-empty answer wins.
pub struct ChainedDiscovery {
  strategies: Vec<Box<dyn Discovery>>,
}

impl ChainedDiscovery {
  /// Chain in priority order.
  pub fn new(strategies: Vec<Box<dyn Discovery>>) -> Self {
    Self { strategies }
  }
}

#[async_trait]
impl Discovery for ChainedDiscovery {
  async fn candidates(&self) -> Vec<String> {
    for strategy in &self.strategies {
      let found = strategy.candidates().await;
      if !found.is_empty() {
        return found;
      }
    }
    Vec::new()
  }
}

/// Kubernetes EndpointSlice discovery (feature `kubernetes`, D18).
#[cfg(feature = "kubernetes")]
mod kubernetes {
  use std::net::{IpAddr, SocketAddr};
  use std::path::{Path, PathBuf};
  use std::sync::Arc;
  use std::time::Duration;

  use async_trait::async_trait;
  use bytes::Bytes;
  use http_body_util::{BodyExt, Empty, Limited};
  use hyper::header::{ACCEPT, AUTHORIZATION, HOST, HeaderValue};
  use hyper_util::rt::TokioIo;
  use rustls::pki_types::pem::PemObject;
  use rustls::pki_types::{CertificateDer, ServerName};
  use serde::Deserialize;

  use super::Discovery;

  /// Where Kubernetes mounts a pod's service account: `token`, `ca.crt` and
  /// `namespace`.
  pub const SERVICE_ACCOUNT_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

  /// Largest EndpointSlice page body read, in bytes.
  const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
  /// EndpointSlices requested per page.
  const PAGE_LIMIT: u32 = 500;
  /// Default bound on one listing, every page included.
  const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

  /// Why a Kubernetes listing (or its setup) failed.
  #[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
  #[non_exhaustive]
  pub enum KubernetesError {
    /// `KUBERNETES_SERVICE_HOST` or `KUBERNETES_SERVICE_PORT` is missing or
    /// malformed: the process does not run in a pod.
    #[error("kubernetes: not in a cluster: {0}")]
    NotInCluster(String),
    /// A service-account file could not be read.
    #[error("kubernetes: cannot read {path}: {message}")]
    Read {
      /// The file.
      path: PathBuf,
      /// The I/O (or encoding) error.
      message: String,
    },
    /// The CA bundle holds no usable certificate.
    #[error("kubernetes: bad CA bundle: {0}")]
    Ca(String),
    /// A namespace or service name is not a DNS label.
    #[error("kubernetes: invalid name {0:?}")]
    InvalidName(String),
    /// The TCP connection to the API server failed.
    #[error("kubernetes: connect failed: {0}")]
    Connect(String),
    /// The TLS handshake failed; an API server certificate not issued by the
    /// cluster CA lands here.
    #[error("kubernetes: TLS: {0}")]
    Tls(String),
    /// The HTTP exchange failed.
    #[error("kubernetes: HTTP: {0}")]
    Http(String),
    /// The API server answered a non-success status (401: token rejected,
    /// 403: the service account may not list EndpointSlices).
    #[error("kubernetes: API server answered {0}")]
    Status(u16),
    /// The body is not an EndpointSliceList, or exceeds the page cap.
    #[error("kubernetes: bad response: {0}")]
    Decode(String),
    /// The listing did not finish in time.
    #[error("kubernetes: timed out after {0:?}")]
    Timeout(Duration),
  }

  /// Ready endpoints of a headless service, read from the API server's
  /// `discovery.k8s.io/v1` EndpointSlices (no kube-rs).
  ///
  /// The client is HTTP/1.1 over rustls and verifies the API server against
  /// the cluster CA (`ca.crt`); there is no way to skip that check. It sends
  /// the service-account `token`, re-read on every listing because projected
  /// tokens rotate, and lists in the pod's own `namespace`. The service
  /// account needs `list` on `endpointslices` in `discovery.k8s.io`.
  ///
  /// An endpoint with `ready: false` or `terminating: true` is skipped;
  /// `ready` unset counts as ready, as the API documents. Each address gets
  /// the mesh `port`; the answer is sorted, so every node sees the same order.
  /// [`Discovery::candidates`] logs a failure and answers empty, so the
  /// strategy composes with [`super::ChainedDiscovery`]; [`Self::endpoints`]
  /// returns the error.
  #[derive(Clone, Debug)]
  pub struct KubernetesDiscovery {
    host: String,
    api_port: u16,
    namespace: String,
    service: String,
    port: u16,
    token_path: PathBuf,
    tls: Arc<rustls::ClientConfig>,
    timeout: Duration,
  }

  impl KubernetesDiscovery {
    /// Discovery inside a pod: the API server from `KUBERNETES_SERVICE_HOST`
    /// and `KUBERNETES_SERVICE_PORT` (443 when unset), credentials from
    /// [`SERVICE_ACCOUNT_DIR`]. `service` names the headless service; `port`
    /// is the mesh port announced to on every address.
    pub fn in_cluster(service: impl Into<String>, port: u16) -> Result<Self, KubernetesError> {
      let host = std::env::var("KUBERNETES_SERVICE_HOST").map_err(|_| {
        KubernetesError::NotInCluster("KUBERNETES_SERVICE_HOST is not set".to_owned())
      })?;
      let api_port = match std::env::var("KUBERNETES_SERVICE_PORT") {
        Ok(raw) => raw.trim().parse().map_err(|_| {
          KubernetesError::NotInCluster(format!("KUBERNETES_SERVICE_PORT {raw:?} is not a port"))
        })?,
        Err(_) => 443,
      };
      Self::from_service_account_dir(host, api_port, SERVICE_ACCOUNT_DIR, service, port)
    }

    /// Discovery against the API server at `api_host:api_port` with the
    /// `token`, `ca.crt` and `namespace` files of `account_dir` (laid out as
    /// [`SERVICE_ACCOUNT_DIR`]). The CA and namespace are read now; the
    /// token on every listing. `api_host` must match a name or IP address
    /// in the API server certificate.
    pub fn from_service_account_dir(
      api_host: impl Into<String>,
      api_port: u16,
      account_dir: impl AsRef<Path>,
      service: impl Into<String>,
      port: u16,
    ) -> Result<Self, KubernetesError> {
      let dir = account_dir.as_ref();
      let service = dns_label(service.into())?;
      let namespace = dns_label(read_trimmed(&dir.join("namespace"))?)?;
      let tls = Arc::new(client_config(&read(&dir.join("ca.crt"))?)?);
      Ok(Self {
        host: api_host.into(),
        api_port,
        namespace,
        service,
        port,
        token_path: dir.join("token"),
        tls,
        timeout: DEFAULT_TIMEOUT,
      })
    }

    /// Bound one listing, every page included (default 10 s).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
      self.timeout = timeout;
      self
    }

    /// The namespace listed (from the service-account `namespace` file).
    pub fn namespace(&self) -> &str {
      &self.namespace
    }

    /// The ready `address:port` candidates, or why they could not be listed.
    pub async fn endpoints(&self) -> Result<Vec<String>, KubernetesError> {
      tokio::time::timeout(self.timeout, self.list())
        .await
        .map_err(|_| KubernetesError::Timeout(self.timeout))?
    }

    async fn list(&self) -> Result<Vec<String>, KubernetesError> {
      let token = read_trimmed(&self.token_path)?;
      let mut auth =
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| KubernetesError::Read {
          path: self.token_path.clone(),
          message: "the token is not a valid header value".to_owned(),
        })?;
      auth.set_sensitive(true);
      let host_header = HeaderValue::from_str(&self.authority()).map_err(http_error)?;

      let server_name = ServerName::try_from(self.host.clone())
        .map_err(|e| KubernetesError::Tls(format!("server name {:?}: {e}", self.host)))?;
      let tcp = tokio::net::TcpStream::connect((self.host.as_str(), self.api_port))
        .await
        .map_err(|e| KubernetesError::Connect(e.to_string()))?;
      let tls = tokio_rustls::TlsConnector::from(Arc::clone(&self.tls))
        .connect(server_name, tcp)
        .await
        .map_err(|e| KubernetesError::Tls(e.to_string()))?;
      let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .map_err(http_error)?;
      let _driver = AbortOnDrop(tokio::spawn(connection));

      let mut found = Vec::new();
      let mut next_page: Option<String> = None;
      loop {
        let mut path = format!(
          "/apis/discovery.k8s.io/v1/namespaces/{}/endpointslices\
           ?labelSelector=kubernetes.io%2Fservice-name%3D{}&limit={PAGE_LIMIT}",
          self.namespace, self.service
        );
        if let Some(page) = &next_page {
          path.push_str("&continue=");
          percent_encode_into(page, &mut path);
        }
        let request = hyper::Request::get(path)
          .header(HOST, host_header.clone())
          .header(AUTHORIZATION, auth.clone())
          .header(ACCEPT, "application/json")
          .body(Empty::<Bytes>::new())
          .map_err(http_error)?;
        sender.ready().await.map_err(http_error)?;
        let response = sender.send_request(request).await.map_err(http_error)?;
        if !response.status().is_success() {
          return Err(KubernetesError::Status(response.status().as_u16()));
        }
        let body = Limited::new(response.into_body(), MAX_PAGE_BYTES)
          .collect()
          .await
          .map_err(|e| KubernetesError::Decode(e.to_string()))?
          .to_bytes();
        let page: SliceList =
          serde_json::from_slice(&body).map_err(|e| KubernetesError::Decode(e.to_string()))?;
        ready_addresses(&page, self.port, &mut found);
        match page.metadata.and_then(|m| m.continue_token) {
          Some(token) if !token.is_empty() => next_page = Some(token),
          _ => break,
        }
      }
      found.sort();
      found.dedup();
      Ok(found)
    }

    /// `host:port` for the `Host` header, bracketing an IPv6 literal.
    fn authority(&self) -> String {
      if self.host.contains(':') {
        format!("[{}]:{}", self.host, self.api_port)
      } else {
        format!("{}:{}", self.host, self.api_port)
      }
    }
  }

  #[async_trait]
  impl Discovery for KubernetesDiscovery {
    async fn candidates(&self) -> Vec<String> {
      match self.endpoints().await {
        Ok(found) => found,
        Err(error) => {
          tracing::warn!(
            %error,
            namespace = %self.namespace,
            service = %self.service,
            "kubernetes discovery failed"
          );
          Vec::new()
        }
      }
    }
  }

  /// Stops the HTTP connection task when the listing ends, early or not.
  struct AbortOnDrop(tokio::task::JoinHandle<hyper::Result<()>>);

  impl Drop for AbortOnDrop {
    fn drop(&mut self) {
      self.0.abort();
    }
  }

  /// The fields of a `discovery.k8s.io/v1` EndpointSliceList that discovery
  /// reads; lists the API sends as `null` are optional.
  #[derive(Deserialize)]
  struct SliceList {
    metadata: Option<ListMeta>,
    items: Option<Vec<Slice>>,
  }

  #[derive(Deserialize)]
  struct ListMeta {
    #[serde(rename = "continue")]
    continue_token: Option<String>,
  }

  #[derive(Deserialize)]
  struct Slice {
    endpoints: Option<Vec<Endpoint>>,
  }

  #[derive(Deserialize)]
  struct Endpoint {
    addresses: Option<Vec<String>>,
    conditions: Option<Conditions>,
  }

  #[derive(Deserialize)]
  struct Conditions {
    ready: Option<bool>,
    terminating: Option<bool>,
  }

  /// Append the `address:port` of every ready, non-terminating endpoint.
  fn ready_addresses(page: &SliceList, port: u16, out: &mut Vec<String>) {
    let endpoints = page
      .items
      .iter()
      .flatten()
      .flat_map(|slice| slice.endpoints.iter().flatten());
    for endpoint in endpoints {
      let (ready, terminating) = endpoint
        .conditions
        .as_ref()
        .map_or((None, None), |c| (c.ready, c.terminating));
      // `ready` unset is an unknown state, which consumers treat as ready.
      if ready == Some(false) || terminating == Some(true) {
        continue;
      }
      for address in endpoint.addresses.iter().flatten() {
        out.push(match address.parse::<IpAddr>() {
          Ok(ip) => SocketAddr::new(ip, port).to_string(),
          Err(_) => format!("{address}:{port}"),
        });
      }
    }
  }

  /// TLS client config trusting only the cluster CA in `ca_pem`.
  fn client_config(ca_pem: &[u8]) -> Result<rustls::ClientConfig, KubernetesError> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(ca_pem) {
      let cert = cert.map_err(|e| KubernetesError::Ca(e.to_string()))?;
      roots
        .add(cert)
        .map_err(|e| KubernetesError::Ca(e.to_string()))?;
    }
    if roots.is_empty() {
      return Err(KubernetesError::Ca("no CERTIFICATE block".to_owned()));
    }
    // The process default provider if installed, else aws-lc-rs (as `transport::certs`).
    let provider = rustls::crypto::CryptoProvider::get_default()
      .cloned()
      .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    Ok(
      rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| KubernetesError::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
  }

  /// `name` if it is a DNS label (namespaces and service names are).
  fn dns_label(name: String) -> Result<String, KubernetesError> {
    let valid = (1..=63).contains(&name.len())
      && name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
      && !name.starts_with('-')
      && !name.ends_with('-');
    if valid {
      Ok(name)
    } else {
      Err(KubernetesError::InvalidName(name))
    }
  }

  fn read(path: &Path) -> Result<Vec<u8>, KubernetesError> {
    std::fs::read(path).map_err(|e| KubernetesError::Read {
      path: path.to_path_buf(),
      message: e.to_string(),
    })
  }

  fn read_trimmed(path: &Path) -> Result<String, KubernetesError> {
    String::from_utf8(read(path)?)
      .map(|text| text.trim().to_owned())
      .map_err(|_| KubernetesError::Read {
        path: path.to_path_buf(),
        message: "not UTF-8".to_owned(),
      })
  }

  fn http_error(error: impl std::fmt::Display) -> KubernetesError {
    KubernetesError::Http(error.to_string())
  }

  /// Percent-encode everything but RFC 3986 unreserved characters.
  fn percent_encode_into(raw: &str, out: &mut String) {
    for byte in raw.bytes() {
      if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
        out.push(char::from(byte));
      } else {
        out.push_str(&format!("%{byte:02X}"));
      }
    }
  }

  #[cfg(test)]
  mod tests {
    use super::*;

    fn page(json: &str) -> SliceList {
      serde_json::from_str(json).expect("slice list")
    }

    #[test]
    fn ready_addresses_skip_not_ready_and_terminating() {
      let list = page(
        r#"{"metadata":{},"items":[{"addressType":"IPv4","endpoints":[
          {"addresses":["10.0.0.1"],"conditions":{"ready":true}},
          {"addresses":["10.0.0.2"],"conditions":{}},
          {"addresses":["10.0.0.3"],"conditions":{"ready":false}},
          {"addresses":["10.0.0.4"],"conditions":{"ready":true,"terminating":true}}
        ]},{"addressType":"IPv6","endpoints":[{"addresses":["fd00::1"]}]},
        {"addressType":"IPv4","endpoints":null}]}"#,
      );
      let mut found = Vec::new();
      ready_addresses(&list, 4369, &mut found);
      assert_eq!(found, ["10.0.0.1:4369", "10.0.0.2:4369", "[fd00::1]:4369"]);
    }

    #[test]
    fn names_must_be_dns_labels() {
      assert!(dns_label("mesh-0".to_owned()).is_ok());
      for bad in [
        "",
        "-mesh",
        "mesh-",
        "Mesh",
        "a/b",
        "a&b=c",
        &"x".repeat(64),
      ] {
        assert_eq!(
          dns_label(bad.to_owned()),
          Err(KubernetesError::InvalidName(bad.to_owned()))
        );
      }
    }

    #[test]
    fn continue_tokens_are_percent_encoded() {
      let mut out = String::new();
      percent_encode_into("eyJ2Ijo+/=", &mut out);
      assert_eq!(out, "eyJ2Ijo%2B%2F%3D");
    }

    #[test]
    fn missing_service_account_files_fail_at_construction() {
      let err = KubernetesDiscovery::from_service_account_dir(
        "127.0.0.1",
        443,
        "/nonexistent/id-effect-service-account",
        "mesh",
        4369,
      )
      .expect_err("no namespace file");
      assert!(matches!(err, KubernetesError::Read { .. }), "{err}");
    }

    #[test]
    fn a_bundle_without_certificates_is_rejected() {
      assert!(matches!(
        client_config(b"not a pem"),
        Err(KubernetesError::Ca(_))
      ));
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn empty_static_list_means_first_node() {
    let none: [&str; 0] = [];
    assert!(StaticDiscovery::new(none).candidates().await.is_empty());
  }

  #[tokio::test]
  async fn chain_returns_the_first_non_empty_answer() {
    let none: [&str; 0] = [];
    let strategies: Vec<Box<dyn Discovery>> = vec![
      Box::new(StaticDiscovery::new(none)),
      Box::new(StaticDiscovery::new(["b:1"])),
      Box::new(StaticDiscovery::new(["c:1"])),
    ];
    let chain = ChainedDiscovery::new(strategies);
    assert_eq!(chain.candidates().await, vec!["b:1"]);
  }

  #[tokio::test]
  async fn chain_of_nothing_is_empty() {
    assert!(
      ChainedDiscovery::new(Vec::new())
        .candidates()
        .await
        .is_empty()
    );
  }

  #[tokio::test]
  async fn dns_resolves_literal_addresses_without_a_lookup() {
    let found = DnsDiscovery::new("127.0.0.1:4000").candidates().await;
    assert_eq!(found, vec!["127.0.0.1:4000"]);
  }

  #[tokio::test]
  async fn dns_failure_yields_no_candidates() {
    // Missing port: rejected before any network I/O.
    assert!(DnsDiscovery::new("no-port").candidates().await.is_empty());
  }
}
