//! The RPC WebSocket transport over TLS HTTP/2 (RFC 8441 extended `CONNECT`, F15,
//! r13-f-ext-connect): the id_effect_axum TLS host serves `GET /rpc/ws` to an HTTP/2 client as
//! an extended `CONNECT` stream, the codec is negotiated by subprotocol as on HTTP/1.1 (no codec
//! offered is still **415**), and `WsClient` speaks every kind over the stream.
#![cfg(all(feature = "ws", feature = "client"))]

use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{Method, Request, StatusCode, Version};
use http_body_util::Empty;
use hyper_util::rt::{TokioExecutor, TokioIo};
use id_effect::{DynEnv, Never, Stream, schema, succeed};
use id_effect_axum::ConnectionLimits;
use id_effect_axum::server::serve_tls_listener;
use id_effect_axum::server::tls::ServerTls;
use id_effect_platform::tls::{Identity, NodeCertificate, provider};
use id_effect_rpc::client::CallEvent;
use id_effect_rpc::client::ws::WsClient;
use id_effect_rpc::transport::ws::RpcWsConfig;
use id_effect_rpc::{Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::Role;

type H2 = hyper::client::conn::http2::SendRequest<Empty<Bytes>>;

fn group() -> RpcGroup {
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Greeter.greet").public(),
    WireSchema::new(schema::string::<()>()),
    WireSchema::new(schema::string::<()>()),
    |name: String, _: &mut DynEnv| succeed::<_, Never, _>(format!("Hello, {name}!")),
  );
  group.server_stream(
    Procedure::server_stream("Count.up").public(),
    WireSchema::new(schema::i64::<()>()),
    WireSchema::new(schema::i64::<()>()),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
  );
  group
}

/// The RPC server on the TLS host (ALPN `h2` and `http/1.1`), and the root trusting it.
async fn serve() -> (std::net::SocketAddr, RootCertStore) {
  let cert = NodeCertificate::self_signed("localhost").unwrap();
  let identity = Identity::from_certificate(&cert).unwrap();
  let tls = ServerTls::from_identity(&identity).unwrap();
  let mut roots = RootCertStore::empty();
  roots
    .add(CertificateDer::from(cert.chain()[0].to_vec()))
    .unwrap();
  let app = RpcServer::new(group())
    .with_websocket(RpcWsConfig::default())
    .router()
    .with_state(DynEnv::new())
    .into_axum();
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(serve_tls_listener(
    listener,
    app,
    ConnectionLimits::default(),
    tls,
    std::future::pending(),
  ));
  (addr, roots)
}

/// An HTTP/2 connection to `addr` whose client has the server's SETTINGS (one round trip).
async fn h2(addr: std::net::SocketAddr, roots: RootCertStore) -> H2 {
  let mut config = ClientConfig::builder_with_provider(provider())
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
  config.alpn_protocols = vec![b"h2".to_vec()];
  let tcp = TcpStream::connect(addr).await.unwrap();
  let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
    .connect(ServerName::try_from("localhost").unwrap(), tcp)
    .await
    .unwrap();
  let (mut send, conn) =
    hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
      .await
      .unwrap();
  tokio::spawn(async move {
    let _ = conn.await;
  });
  let res = send
    .send_request(
      Request::get("https://localhost/rpc/ws")
        .body(Empty::new())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.version(), Version::HTTP_2);
  send
}

/// `CONNECT :protocol websocket` on `/rpc/ws` offering `protocols`: the response.
async fn connect(
  send: &mut H2,
  protocols: Option<&'static str>,
) -> hyper::Response<hyper::body::Incoming> {
  let mut req = Request::builder()
    .method(Method::CONNECT)
    .uri("https://localhost/rpc/ws")
    .header("sec-websocket-version", "13");
  if let Some(protocols) = protocols {
    req = req.header("sec-websocket-protocol", protocols);
  }
  let mut req = req.body(Empty::new()).unwrap();
  req
    .extensions_mut()
    .insert(hyper::ext::Protocol::from_static("websocket"));
  send.send_request(req).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rpc_over_h2_websocket() {
  let (addr, roots) = serve().await;
  let mut send = h2(addr, roots).await;

  // No codec offered: refused as on HTTP/1.1, no silent JSON fallback.
  let refused = connect(&mut send, None).await;
  assert_eq!(refused.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

  let res = connect(&mut send, Some("idrpc.json.v1")).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(res.headers()["sec-websocket-protocol"], "idrpc.json.v1");
  let upgraded = hyper::upgrade::on(res).await.unwrap();
  let socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await;
  let client = WsClient::over(socket).await.unwrap();
  assert_eq!(client.hello().codec, "idrpc.json.v1");
  assert_eq!(
    client.call("Greeter.greet", json!("h2")).await.unwrap(),
    json!("Hello, h2!")
  );
  let mut up = client.open("Count.up", json!(3), None).await.unwrap();
  let mut items = Vec::new();
  loop {
    match up.next().await {
      Some(CallEvent::Item { value, .. }) => items.push(value),
      Some(CallEvent::Exit(result)) => {
        assert_eq!(result.unwrap(), Value::Null);
        break;
      }
      None => panic!("no exit"),
    }
  }
  assert_eq!(items, [json!(1), json!(2), json!(3)]);

  // A second RPC socket multiplexed on the same HTTP/2 connection.
  let res = connect(&mut send, Some("idrpc.json.v1")).await;
  let upgraded = hyper::upgrade::on(res).await.unwrap();
  let socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await;
  let second = WsClient::over(socket).await.unwrap();
  assert_ne!(second.hello().session, client.hello().session);
  assert_eq!(
    second.call("Greeter.greet", json!("again")).await.unwrap(),
    json!("Hello, again!")
  );
}
