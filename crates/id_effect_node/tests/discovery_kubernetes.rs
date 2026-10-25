//! Kubernetes discovery (dist.membership #4, D18) against an in-process fake
//! API server: HTTPS with a throwaway cluster CA, EndpointSlice JSON, and a
//! bearer-token check.

#![cfg(feature = "kubernetes")]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use id_effect_node::discovery::{
  ChainedDiscovery, Discovery, KubernetesDiscovery, KubernetesError, StaticDiscovery,
};
use serde_json::json;

const NAMESPACE: &str = "team-a";
const SERVICE: &str = "mesh";
const MESH_PORT: u16 = 4369;
const TOKEN: &str = "sa-token-1";

/// A throwaway cluster CA.
struct Ca {
  cert: rcgen::Certificate,
  issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

impl Ca {
  fn new(name: &str) -> Self {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
      .distinguished_name
      .push(rcgen::DnType::CommonName, name);
    params.key_usages = vec![
      rcgen::KeyUsagePurpose::KeyCertSign,
      rcgen::KeyUsagePurpose::CrlSign,
      rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let key = rcgen::KeyPair::generate().expect("ca key");
    let cert = params.self_signed(&key).expect("ca cert");
    let issuer = rcgen::Issuer::new(params, key);
    Self { cert, issuer }
  }

  /// The API server's TLS config: a leaf for `127.0.0.1` issued by this CA.
  fn server_config(&self) -> rustls::ServerConfig {
    let key = rcgen::KeyPair::generate().expect("leaf key");
    let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()]).expect("params");
    params
      .distinguished_name
      .push(rcgen::DnType::CommonName, "kube-apiserver");
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = params.signed_by(&key, &self.issuer).expect("leaf cert");
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    rustls::ServerConfig::builder_with_provider(provider)
      .with_safe_default_protocol_versions()
      .expect("versions")
      .with_no_client_auth()
      .with_single_cert(
        vec![leaf.der().clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
      )
      .expect("server cert")
  }
}

/// One request the fake API server received.
#[derive(Clone, Debug)]
struct Seen {
  path_and_query: String,
  authorization: Option<String>,
}

/// An HTTPS API server answering EndpointSlice lists with `slices` to a
/// request bearing [`TOKEN`], 401 otherwise.
struct FakeApi {
  addr: SocketAddr,
  seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeApi {
  async fn start(ca: &Ca, slices: serde_json::Value) -> Self {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
      .await
      .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(ca.server_config()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let body = Bytes::from(
      serde_json::to_vec(&json!({
        "kind": "EndpointSliceList",
        "apiVersion": "discovery.k8s.io/v1",
        "metadata": { "resourceVersion": "1" },
        "items": slices,
      }))
      .expect("json"),
    );
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
      loop {
        let Ok((tcp, _)) = listener.accept().await else {
          return;
        };
        let acceptor = acceptor.clone();
        let log = Arc::clone(&log);
        let body = body.clone();
        tokio::spawn(async move {
          // An untrusting client aborts here; nothing reaches the log.
          let Ok(tls) = acceptor.accept(tcp).await else {
            return;
          };
          let service = hyper::service::service_fn(move |request: Request<Incoming>| {
            let log = Arc::clone(&log);
            let body = body.clone();
            async move { Ok::<_, Infallible>(answer(&request, &log, body)) }
          });
          let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(tls), service)
            .await;
        });
      }
    });
    Self { addr, seen }
  }

  fn seen(&self) -> Vec<Seen> {
    self.seen.lock().expect("log").clone()
  }
}

fn answer(
  request: &Request<Incoming>,
  log: &Mutex<Vec<Seen>>,
  body: Bytes,
) -> Response<Full<Bytes>> {
  let authorization = request
    .headers()
    .get(hyper::header::AUTHORIZATION)
    .and_then(|v| v.to_str().ok())
    .map(str::to_owned);
  let authorized = authorization.as_deref() == Some(&format!("Bearer {TOKEN}"));
  log.lock().expect("log").push(Seen {
    path_and_query: request
      .uri()
      .path_and_query()
      .map(|p| p.as_str().to_owned())
      .unwrap_or_default(),
    authorization,
  });
  let expected_path = format!("/apis/discovery.k8s.io/v1/namespaces/{NAMESPACE}/endpointslices");
  let (status, body) = if !authorized {
    (StatusCode::UNAUTHORIZED, Bytes::from_static(b"{}"))
  } else if request.uri().path() != expected_path {
    (StatusCode::NOT_FOUND, Bytes::from_static(b"{}"))
  } else {
    (StatusCode::OK, body)
  };
  Response::builder()
    .status(status)
    .header(hyper::header::CONTENT_TYPE, "application/json")
    .body(Full::new(body))
    .expect("response")
}

/// A service-account mount (`token`, `ca.crt`, `namespace`) trusting `ca`.
fn service_account(test: &str, ca: &Ca) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("id-effect-k8s-{}-{test}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("dir");
  std::fs::write(dir.join("token"), format!("{TOKEN}\n")).expect("token");
  std::fs::write(dir.join("ca.crt"), ca.cert.pem()).expect("ca.crt");
  std::fs::write(dir.join("namespace"), NAMESPACE).expect("namespace");
  dir
}

fn discovery(api: &FakeApi, account: &Path) -> KubernetesDiscovery {
  KubernetesDiscovery::from_service_account_dir(
    "127.0.0.1",
    api.addr.port(),
    account,
    SERVICE,
    MESH_PORT,
  )
  .expect("discovery")
}

fn endpoint(address: &str, ready: Option<bool>) -> serde_json::Value {
  let conditions = match ready {
    Some(ready) => json!({ "ready": ready, "serving": ready, "terminating": false }),
    None => json!({}),
  };
  json!({ "addresses": [address], "conditions": conditions })
}

fn slice(name: &str, endpoints: Vec<serde_json::Value>) -> serde_json::Value {
  json!({
    "metadata": {
      "name": name,
      "namespace": NAMESPACE,
      "labels": { "kubernetes.io/service-name": SERVICE },
    },
    "addressType": "IPv4",
    "endpoints": endpoints,
    "ports": [{ "name": "mesh", "port": MESH_PORT, "protocol": "TCP" }],
  })
}

#[tokio::test]
async fn kubernetes_discovery_lists_ready_endpoints() {
  let ca = Ca::new("cluster ca");
  let api = FakeApi::start(
    &ca,
    json!([
      slice(
        "mesh-abc",
        vec![endpoint("10.0.1.7", Some(true)), endpoint("10.0.1.5", None)]
      ),
      slice("mesh-def", vec![endpoint("10.0.2.9", Some(true))]),
    ]),
  )
  .await;
  let account = service_account("lists", &ca);
  let k8s = discovery(&api, &account);
  assert_eq!(k8s.namespace(), NAMESPACE);

  let expected = vec!["10.0.1.5:4369", "10.0.1.7:4369", "10.0.2.9:4369"];
  assert_eq!(k8s.endpoints().await.expect("endpoints"), expected);

  // A Discovery strategy like the others: the chain falls through an empty
  // static list to the cluster's answer.
  let none: [&str; 0] = [];
  let chain = ChainedDiscovery::new(vec![Box::new(StaticDiscovery::new(none)), Box::new(k8s)]);
  assert_eq!(chain.candidates().await, expected);
}

#[tokio::test]
async fn kubernetes_discovery_skips_not_ready() {
  let ca = Ca::new("cluster ca");
  let api = FakeApi::start(
    &ca,
    json!([slice(
      "mesh-abc",
      vec![
        endpoint("10.0.1.1", Some(true)),
        endpoint("10.0.1.2", Some(false)),
        json!({
          "addresses": ["10.0.1.3"],
          "conditions": { "ready": true, "serving": true, "terminating": true },
        }),
      ]
    )]),
  )
  .await;
  let account = service_account("not-ready", &ca);

  assert_eq!(
    discovery(&api, &account).candidates().await,
    vec!["10.0.1.1:4369"]
  );
}

#[tokio::test]
async fn kubernetes_discovery_uses_service_account_token() {
  let ca = Ca::new("cluster ca");
  let api = FakeApi::start(
    &ca,
    json!([slice("mesh-abc", vec![endpoint("10.0.1.1", Some(true))])]),
  )
  .await;
  let account = service_account("token", &ca);
  let k8s = discovery(&api, &account);

  assert_eq!(
    k8s.endpoints().await.expect("endpoints"),
    vec!["10.0.1.1:4369"]
  );
  let seen = api.seen();
  assert_eq!(seen.len(), 1);
  assert_eq!(
    seen[0].authorization.as_deref(),
    Some("Bearer sa-token-1"),
    "the mounted token, trimmed"
  );
  assert_eq!(
    seen[0].path_and_query,
    "/apis/discovery.k8s.io/v1/namespaces/team-a/endpointslices\
     ?labelSelector=kubernetes.io%2Fservice-name%3Dmesh&limit=500",
    "the namespace from the mount, the service's label selector"
  );

  // The token is re-read on every listing (projected tokens rotate): the
  // server sees the new one, and rejects it.
  std::fs::write(account.join("token"), "sa-token-2").expect("rotate");
  assert_eq!(k8s.endpoints().await, Err(KubernetesError::Status(401)));
  assert_eq!(
    api.seen()[1].authorization.as_deref(),
    Some("Bearer sa-token-2")
  );
  assert!(k8s.candidates().await.is_empty());
}

#[tokio::test]
async fn kubernetes_discovery_rejects_untrusted_api_cert() {
  let cluster = Ca::new("cluster ca");
  let impostor = Ca::new("impostor ca");
  let api = FakeApi::start(
    &impostor,
    json!([slice("mesh-abc", vec![endpoint("10.6.6.6", Some(true))])]),
  )
  .await;
  let account = service_account("untrusted", &cluster);
  let k8s = discovery(&api, &account);

  let err = k8s.endpoints().await.expect_err("untrusted API server");
  assert!(matches!(err, KubernetesError::Tls(_)), "{err}");
  assert!(k8s.candidates().await.is_empty());
  assert!(api.seen().is_empty(), "no request, so no token, was sent");
}
