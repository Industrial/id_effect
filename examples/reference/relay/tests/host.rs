//! F59 (0.13 part): the Relay serves HTTP/3 and WebTransport from the host entry point
//! (`ServerHandle` with `server.http3` and `server.http3.webtransport` on), not from a QUIC
//! endpoint of its own; the mesh has its own endpoint (`idm/1`). The Rust WebSocket client dials
//! the same port over `wss://` itself (F40). No database is needed: nothing here reads one.

mod common;

use std::sync::Arc;

use common::{HOST, Http3Only, Wt, http_config, mesh_pki, node_config, run};
use id_effect_kit::platform::http::{Http3Client, HttpClient as _, HttpRequest, ReqwestHttpClient};
use id_effect_kit::platform::tls::NodeCertificate;
use id_effect_kit::sql::{SqlClient, TestSqlClient};
use relay::node::RelayNode;

/// The node's source: the host serves HTTP/3, so no `serve_h3` endpoint of the Relay's own.
const NODE_RS: &str = include_str!("../src/node.rs");

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_uses_host_webtransport() {
  assert!(
    !NODE_RS.contains("serve_h3"),
    "the Relay serves HTTP/3 through the host, not an endpoint of its own"
  );
  let certificate = NodeCertificate::self_signed(HOST).unwrap();
  let db: Arc<dyn SqlClient> = Arc::new(TestSqlClient::new());
  let options = common::Options {
    nodes: 1,
    ..common::Options::default()
  };
  let node = RelayNode::start(node_config(
    0,
    &db,
    &certificate,
    mesh_pki(1).remove(0),
    &options,
  ))
  .await
  .expect("node starts");

  // The host's settings: HTTP/3 and WebTransport on, TLS with the node's certificate.
  let host = node.host_config();
  assert!(host.http3, "host HTTP/3 on");
  assert!(host.webtransport, "host WebTransport on");
  assert!(host.tls.is_some(), "host TLS");
  assert_eq!(host.bind_port, node.addr().port());
  assert!(host.posture_warnings().is_empty(), "{host:?}");
  // The mesh is elsewhere: its own endpoint, its own port.
  assert_ne!(node.mesh_addr().port(), node.addr().port());

  // TCP: the host's `Alt-Svc` names its own HTTP/3 port, the TCP one.
  let tcp = ReqwestHttpClient::from_config(&http_config(&certificate));
  let page = format!("https://{HOST}:{}/health", node.addr().port());
  let response = run(tcp.execute(HttpRequest::get(page.clone())))
    .await
    .unwrap();
  assert_eq!(response.status, 200);
  let alt_svc = response
    .headers
    .iter()
    .find(|(name, _)| name.eq_ignore_ascii_case("alt-svc"))
    .map(|(_, value)| value.clone())
    .expect("the host advertises HTTP/3");
  assert!(
    alt_svc.starts_with(&format!("h3=\":{}\"", node.addr().port())),
    "{alt_svc}"
  );

  // UDP, the same port: HTTP/3 from the host's endpoint ...
  let h3 = Http3Only {
    client: Http3Client::from_config(&http_config(&certificate).http3(true)),
    port: node.addr().port(),
  };
  let response = run(h3.execute(HttpRequest::get(page))).await.unwrap();
  assert_eq!(response.status, 200);
  assert_eq!(&response.body[..], b"ok");

  // ... and a WebTransport session on `/rpc/wt`, signed in: the RPC `hello` on its control
  // stream.
  let token = common::token(&common::keys(), "ada").await;
  let wt = Wt::connect(node.addr(), &certificate, Some(&token))
    .await
    .expect("a WebTransport session on the host's endpoint");
  assert!(wt.hello.is_object(), "{}", wt.hello);
  wt.close();
  // The mesh endpoint speaks `idm/1` only: no HTTP/3, no WebTransport there.
  let refused = tokio::time::timeout(
    common::WAIT,
    Wt::connect(node.mesh_addr(), &certificate, Some(&token)),
  )
  .await
  .expect("an answer in time");
  assert!(refused.is_err(), "WebTransport on the mesh port");

  // F40: the Rust client dials `wss://` itself, trusting the node's certificate ...
  let ws = common::wss(node.addr(), &certificate, &token)
    .await
    .expect("wss with the node's certificate trusted");
  drop(ws);
  // ... and refuses a server whose certificate it does not trust.
  let stranger = NodeCertificate::self_signed(HOST).unwrap();
  let untrusted = common::wss(node.addr(), &stranger, &token)
    .await
    .expect_err("an untrusted server certificate");
  assert!(
    untrusted.contains("certificate") || untrusted.contains("Certificate"),
    "{untrusted}"
  );
  node.shutdown().await;
}
