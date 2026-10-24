//! WebTransport in the host entry points (feature `webtransport`, r13-f-webtransport-host, 0.12
//! follow-up F43): `ServerHandle` and `serve_router` serve the app's WebTransport routes on the
//! host's own HTTP/3 endpoint when `server.http3.webtransport = true`, and only then (ADR 0013
//! R9: off by default); the route's `Origin` allowlist and session caps apply there (R9, R4),
//! and a client whose session is lost opens a new one on the same endpoint and carries on.
#![cfg(feature = "webtransport")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use id_effect::{DynEnv, Never, from_async};
use id_effect_axum::route::Router as TypedRouter;
use id_effect_axum::server::tls::ServerTls;
use id_effect_axum::server::webtransport::client::{
  self, ConnectError, ConnectRequest, WebTransportClient,
};
use id_effect_axum::server::webtransport::{
  self, SessionCap, WebTransportConfig, WebTransportSession, WebTransportStream,
};
use id_effect_axum::server::{HostBuilder, HostTls, WEBTRANSPORT_KEY, bootstrap_env, serve_router};
use id_effect_axum::{HostConfig, ReadinessState, ServerHandle};
use id_effect_config::MapConfigProvider;
use id_effect_platform::tls::{Identity, NodeCertificate, provider};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_rustls::rustls::{self, ClientConfig, RootCertStore};

/// Generous: the suite runs beside compilations that load every core.
const WAIT: Duration = Duration::from_secs(20);

/// The browser origin the route admits.
const APP_ORIGIN: &str = "https://app.example";

// --- certificate and client --------------------------------------------------------------------

/// A self-signed `localhost` certificate as host TLS files, and the client trust for it.
struct Cert {
  tls: HostTls,
  cert_file: PathBuf,
  key_file: PathBuf,
  roots: RootCertStore,
  identity: Identity,
}

fn cert() -> Cert {
  static NEXT: AtomicUsize = AtomicUsize::new(0);
  let issued = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
  let dir = std::env::temp_dir().join(format!(
    "id-effect-host-wt-{}-{}",
    std::process::id(),
    NEXT.fetch_add(1, Ordering::SeqCst)
  ));
  std::fs::create_dir_all(&dir).unwrap();
  let (cert_file, key_file) = (dir.join("cert.pem"), dir.join("key.pem"));
  std::fs::write(&cert_file, issued.cert.pem()).unwrap();
  std::fs::write(&key_file, issued.signing_key.serialize_pem()).unwrap();
  let mut roots = RootCertStore::empty();
  roots.add(issued.cert.der().clone()).unwrap();
  let node = NodeCertificate::from_pem(
    issued.cert.pem().as_bytes(),
    issued.signing_key.serialize_pem().as_bytes(),
  )
  .unwrap();
  Cert {
    tls: HostTls::files(&cert_file, &key_file),
    cert_file,
    key_file,
    roots,
    identity: Identity::from_certificate(&node).unwrap(),
  }
}

fn client_tls(roots: &RootCertStore) -> ClientConfig {
  ClientConfig::builder_with_provider(provider())
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots.clone())
    .with_no_client_auth()
}

fn from(origin: &str) -> ConnectRequest {
  ConnectRequest {
    origin: Some(origin.to_owned()),
    headers: Vec::new(),
  }
}

async fn connect(
  addr: SocketAddr,
  roots: &RootCertStore,
  request: &ConnectRequest,
) -> Result<WebTransportClient, ConnectError> {
  tokio::time::timeout(
    WAIT,
    client::connect(addr, "localhost", client_tls(roots), "/wt", request),
  )
  .await
  .unwrap_or_else(|_| Err(ConnectError::Transport("timed out".into())))
}

/// One stream: `bytes` out, everything the server answers back.
async fn exchange(session: &WebTransportClient, bytes: &[u8]) -> Vec<u8> {
  let mut stream = session.open_bi().await.unwrap();
  stream.send.write_all(bytes).await.unwrap();
  stream.send.shutdown().await.unwrap();
  let mut out = Vec::new();
  tokio::time::timeout(WAIT, stream.recv.read_to_end(&mut out))
    .await
    .unwrap()
    .unwrap();
  out
}

// --- the app -----------------------------------------------------------------------------------

/// What sessions leave behind for the next one (a resume point per client key).
type Store = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Answers one stream: `put:KEY:DATA` keeps DATA under KEY (answer `ok`), `get:KEY` answers it,
/// `lose` ends the whole session from the server (a lost session), anything else is echoed.
async fn answer(stream: WebTransportStream, store: &Store, session: &WebTransportSession) {
  let WebTransportStream { mut send, mut recv } = stream;
  let mut buf = Vec::new();
  let _ = recv.read_to_end(&mut buf).await;
  let text = String::from_utf8_lossy(&buf).into_owned();
  let reply = if let Some(rest) = text.strip_prefix("put:") {
    let (key, data) = rest.split_once(':').unwrap_or((rest, ""));
    store
      .lock()
      .unwrap()
      .insert(key.to_owned(), data.as_bytes().to_vec());
    b"ok".to_vec()
  } else if let Some(key) = text.strip_prefix("get:") {
    store.lock().unwrap().get(key).cloned().unwrap_or_default()
  } else if text == "lose" {
    session.close(0x10c, "lost");
    return;
  } else {
    buf
  };
  let _ = send.write_all(&reply).await;
  let _ = send.shutdown().await;
}

/// The app: a WebTransport route at `/wt` (admitting [`APP_ORIGIN`], one session per client IP
/// on `cap`) and a plain `GET /health`.
fn app(cap: SessionCap, store: Store) -> Router {
  let config = WebTransportConfig::default()
    .allowed_origins([APP_ORIGIN])
    .unwrap()
    .session_cap(cap);
  let route = webtransport::endpoint(
    "/wt",
    config,
    move |_: &mut DynEnv, _caps, session: WebTransportSession| {
      let store = Arc::clone(&store);
      from_async(move || async move {
        let mut session = session;
        // One stream after the other: the tests exchange them in turn.
        while let Some(stream) = session.accept_bi().await {
          answer(stream, &store, &session).await;
        }
        Ok::<_, Never>(())
      })
    },
  )
  .public();
  TypedRouter::new()
    .route(route)
    .with_state(DynEnv::new())
    .into_axum()
    .merge(id_effect_axum::health_router())
}

fn store() -> Store {
  Arc::default()
}

/// `127.0.0.1`, any port, TLS from `cert`.
fn local(cert: &Cert) -> HostConfig {
  HostConfig {
    bind_host: "127.0.0.1".into(),
    bind_port: 0,
    shutdown_timeout: Duration::from_secs(5),
    ..HostConfig::default()
  }
  .with_tls(cert.tls.clone())
}

async fn start(config: &HostConfig, app: Router) -> ServerHandle {
  ServerHandle::start_with_readiness(config, app, ReadinessState::new(true))
    .await
    .unwrap()
}

/// Waits until no session of `cap` is open.
async fn all_closed(cap: &SessionCap) {
  tokio::time::timeout(WAIT, async {
    while cap.open() > 0 {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await
  .expect("every session slot released");
}

/// A TCP port that was free a moment ago (for `serve_router`, whose `PORT` cannot be `0`).
fn free_port() -> u16 {
  let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
  tcp.local_addr().unwrap().port()
}

// --- tests -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_serves_webtransport_when_enabled() {
  let cert = cert();
  let cap = SessionCap::new(16, 4);
  // ServerHandle: the host's own HTTP/3 endpoint takes the session, on the TCP port number.
  let config = local(&cert).with_webtransport();
  assert!(config.posture_warnings().is_empty());
  let handle = start(&config, app(cap.clone(), store())).await;
  let addr = handle.local_addr();
  let session = connect(addr, &cert.roots, &from(APP_ORIGIN)).await.unwrap();
  assert_eq!(exchange(&session, b"hello").await, b"hello");
  let (a, b) = (
    exchange(&session, b"one").await,
    exchange(&session, b"two").await,
  );
  assert_eq!((a.as_slice(), b.as_slice()), (&b"one"[..], &b"two"[..]));
  assert_eq!(cap.open(), 1);
  // Shutdown closes the session's connection.
  handle.shutdown().await;
  tokio::time::timeout(WAIT, session.closed()).await.unwrap();
  all_closed(&cap).await;

  // serve_router, configured from `server.http3.webtransport` (the environment form).
  let port = free_port();
  let port_text = port.to_string();
  let (cert_file, key_file) = (
    cert.cert_file.display().to_string(),
    cert.key_file.display().to_string(),
  );
  let env_key = WEBTRANSPORT_KEY.replace('.', "_");
  let provider = MapConfigProvider::from_pairs([
    ("HOST", "127.0.0.1"),
    ("PORT", port_text.as_str()),
    ("TLS_CERT_FILE", cert_file.as_str()),
    ("TLS_KEY_FILE", key_file.as_str()),
    (env_key.as_str(), "true"),
  ]);
  let host = HostBuilder::new()
    .with_env(bootstrap_env(provider))
    .build()
    .await
    .unwrap();
  assert!(host.config.webtransport);
  let served = tokio::spawn(serve_router(host, app(cap.clone(), store())));
  let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
  let session = tokio::time::timeout(WAIT, async {
    loop {
      match connect(addr, &cert.roots, &from(APP_ORIGIN)).await {
        Ok(session) => break session,
        Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
      }
    }
  })
  .await
  .expect("serve_router serves WebTransport");
  assert_eq!(
    exchange(&session, b"via serve_router").await,
    b"via serve_router"
  );
  served.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_webtransport_off_by_default() {
  // The secure default: off in the type, and off when the environment says nothing.
  assert!(!HostConfig::default().webtransport);
  let cert = cert();
  let (cert_file, key_file) = (
    cert.cert_file.display().to_string(),
    cert.key_file.display().to_string(),
  );
  let host = HostBuilder::new()
    .with_env(bootstrap_env(MapConfigProvider::from_pairs([
      ("TLS_CERT_FILE", cert_file.as_str()),
      ("TLS_KEY_FILE", key_file.as_str()),
    ])))
    .build()
    .await
    .unwrap();
  assert!(!host.config.webtransport);
  assert!(host.config.http3);
  // A host serving HTTP/3 and an app with a WebTransport route: no session starts.
  let cap = SessionCap::new(16, 4);
  let config = local(&cert);
  let handle = start(&config, app(cap.clone(), store())).await;
  let refused = connect(handle.local_addr(), &cert.roots, &from(APP_ORIGIN)).await;
  assert!(refused.is_err(), "a session started with WebTransport off");
  assert_eq!(cap.open(), 0);
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_webtransport_with_provided_tls_needs_its_identity() {
  let cert = cert();
  let cap = SessionCap::new(16, 4);
  // Settings built from an identity carry it: the host serves WebTransport with it.
  let tls = ServerTls::from_identity(&cert.identity).unwrap();
  let config = HostConfig {
    tls: Some(HostTls::provided(tls)),
    ..local(&cert)
  }
  .with_webtransport();
  let handle = start(&config, app(cap.clone(), store())).await;
  let session = connect(handle.local_addr(), &cert.roots, &from(APP_ORIGIN))
    .await
    .unwrap();
  assert_eq!(exchange(&session, b"provided").await, b"provided");
  handle.shutdown().await;
  // Settings without one (a resolver of your own): TCP only, and no session.
  let bare = ServerTls::builder(Arc::new(cert.identity.clone()))
    .build()
    .unwrap();
  let config = HostConfig {
    tls: Some(HostTls::provided(bare)),
    ..local(&cert)
  }
  .with_webtransport();
  let handle = start(&config, app(cap.clone(), store())).await;
  // Nothing listens on UDP: the handshake goes unanswered (give up after a few seconds).
  let request = from(APP_ORIGIN);
  let attempt = client::connect(
    handle.local_addr(),
    "localhost",
    client_tls(&cert.roots),
    "/wt",
    &request,
  );
  let refused = tokio::time::timeout(Duration::from_secs(3), attempt).await;
  assert!(
    !matches!(refused, Ok(Ok(_))),
    "a session started without an HTTP/3 identity"
  );
  assert_eq!(cap.open(), 0);
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r9_host_webtransport_foreign_origin_refused() {
  let cert = cert();
  // One session per client IP (R4), as on any WebTransport endpoint.
  let cap = SessionCap::new(16, 1);
  let handle = start(&local(&cert).with_webtransport(), app(cap.clone(), store())).await;
  let addr = handle.local_addr();
  // R9: a foreign browser origin is refused before the session starts.
  let foreign = connect(addr, &cert.roots, &from("https://evil.example")).await;
  assert_eq!(foreign.unwrap_err(), ConnectError::Refused(403));
  let null = connect(addr, &cert.roots, &from("null")).await;
  assert_eq!(null.unwrap_err(), ConnectError::Refused(403));
  assert_eq!(cap.open(), 0);
  // The allowed origin gets its session; a second one from the same IP is over the cap.
  let session = connect(addr, &cert.roots, &from(APP_ORIGIN)).await.unwrap();
  assert_eq!(exchange(&session, b"allowed").await, b"allowed");
  let over = connect(addr, &cert.roots, &from(APP_ORIGIN)).await;
  assert_eq!(over.unwrap_err(), ConnectError::Refused(429));
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_webtransport_resume_after_session_loss() {
  let cert = cert();
  let cap = SessionCap::new(16, 1);
  let handle = start(&local(&cert).with_webtransport(), app(cap.clone(), store())).await;
  let addr = handle.local_addr();
  let first = connect(addr, &cert.roots, &from(APP_ORIGIN)).await.unwrap();
  assert_eq!(exchange(&first, b"put:client-1:cursor-7").await, b"ok");
  // The session is lost: the server side ends it under the client.
  let mut lose = first.open_bi().await.unwrap();
  lose.send.write_all(b"lose").await.unwrap();
  lose.send.shutdown().await.unwrap();
  tokio::time::timeout(WAIT, first.closed()).await.unwrap();
  // Its slot is released (one session per IP here), so the client opens a new session on the
  // same host endpoint and picks up where it left off.
  all_closed(&cap).await;
  let second = connect(addr, &cert.roots, &from(APP_ORIGIN)).await.unwrap();
  assert_eq!(exchange(&second, b"get:client-1").await, b"cursor-7");
  assert_eq!(exchange(&second, b"again").await, b"again");
  // The client drops its session in turn: again the slot comes back.
  second.close(0, "done");
  all_closed(&cap).await;
  let third = connect(addr, &cert.roots, &from(APP_ORIGIN)).await.unwrap();
  assert_eq!(exchange(&third, b"get:client-1").await, b"cursor-7");
  handle.shutdown().await;
}
