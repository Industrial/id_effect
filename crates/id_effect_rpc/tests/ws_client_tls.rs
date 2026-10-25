//! `WsClient` over `wss://` and postcard (F40, r13-f-ws-client-tls; ADR 0013 R8): the client
//! dials TLS itself with the platform verifier plus the private CAs a config adds, refuses
//! untrusted and misnamed certificates (there is no "accept invalid certificates" switch), keeps
//! JSON as the default codec, and speaks `idrpc.postcard.v1` through the service's typed codecs
//! when asked. Real sockets on loopback; a throwaway certificate for `localhost`.
#![cfg(all(feature = "ws", feature = "client"))]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::request::Parts;
use id_effect::{DynEnv, Never, Stream, schema, succeed};
use id_effect_axum::route::{GuardFuture, RouteGuard};
use id_effect_platform::tls::{Identity, NodeCertificate, server_tls_config_with};
use id_effect_rpc::client::ws::{WsCall, WsClient, WsClientConfig};
use id_effect_rpc::client::{CallEvent, RpcClientError};
use id_effect_rpc::codec::CodecId;
use id_effect_rpc::kinds::{Subscription, SubscriptionItem};
use id_effect_rpc::transport::ws::RpcWsConfig;
use id_effect_rpc::wire::Cursor;
use id_effect_rpc::{Duplex, Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// The schemas: typed (`with_serde`) when postcard is built, so one group serves both codecs.
fn typed<T>(schema: WireSchema<T>) -> WireSchema<T>
where
  T: serde::Serialize + serde::de::DeserializeOwned + 'static,
{
  #[cfg(feature = "postcard")]
  {
    schema.with_serde()
  }
  #[cfg(not(feature = "postcard"))]
  {
    schema
  }
}

fn int() -> WireSchema<i64> {
  typed(WireSchema::new(schema::i64::<()>()))
}

fn text() -> WireSchema<String> {
  typed(WireSchema::new(schema::string::<()>()))
}

/// Admits requests with `authorization: Bearer ok` on the upgrade.
struct Bearer;

impl RouteGuard for Bearer {
  fn policy(&self) -> &str {
    "bearer"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let ok = parts
      .headers
      .get("authorization")
      .is_some_and(|v| v == "Bearer ok");
    Box::pin(async move {
      if ok {
        Ok(())
      } else {
        Err(axum::response::IntoResponse::into_response(
          axum::http::StatusCode::FORBIDDEN,
        ))
      }
    })
  }
}

fn group() -> RpcGroup {
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Greeter.greet").public(),
    text(),
    text(),
    |name: String, _: &mut DynEnv| succeed::<_, Never, _>(format!("Hello, {name}!")),
  );
  group.mutation(
    Procedure::mutation("Counter.add").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n + 1),
  );
  group.server_stream(
    Procedure::server_stream("Count.up").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
  );
  group.client_stream(
    Procedure::client_stream("Sum.all").public(),
    int(),
    int(),
    |items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| items.run_fold(0, |a, b| a + b),
  );
  group.bidi(
    Procedure::bidi("Shout.back").public(),
    text(),
    text(),
    |_: &mut DynEnv| {
      succeed::<_, Never, _>(Duplex::new(|items: Stream<String, Never, DynEnv>| {
        items.map(|s| s.to_uppercase())
      }))
    },
  );
  group.subscription(
    Procedure::subscription("Feed.watch").public(),
    text(),
    text(),
    |prefix: String, _after: Option<Cursor>, _: &mut DynEnv| {
      let items = (1..=2_u64)
        .map(|n| SubscriptionItem::new(format!("{prefix}{n}")).with_cursor(Cursor::events(n)))
        .collect();
      succeed::<_, Never, _>(Subscription::new(Stream::from_vec(items)))
    },
  );
  group.query(
    Procedure::query("Secret.read").guard(Bearer),
    text(),
    text(),
    |s: String, _: &mut DynEnv| succeed::<_, Never, _>(s),
  );
  // JSON-only schemas: postcard refuses it (415).
  group.query(
    Procedure::query("Plain.echo").public(),
    WireSchema::new(schema::string::<()>()),
    WireSchema::new(schema::string::<()>()),
    |s: String, _: &mut DynEnv| succeed::<_, Never, _>(s),
  );
  group
}

fn app() -> axum::Router {
  // The realtime layer's default message rate is plenty for these few calls.
  let server = RpcServer::new(group()).with_websocket(RpcWsConfig::default());
  server.router().with_state(DynEnv::new()).into_axum()
}

/// TLS over a TCP listener for `axum::serve`; failed handshakes are dropped (the client
/// refused the certificate).
struct TlsListener {
  tcp: TcpListener,
  acceptor: TlsAcceptor,
}

impl axum::serve::Listener for TlsListener {
  type Io = tokio_rustls::server::TlsStream<TcpStream>;
  type Addr = SocketAddr;

  async fn accept(&mut self) -> (Self::Io, Self::Addr) {
    loop {
      let Ok((tcp, addr)) = self.tcp.accept().await else {
        continue;
      };
      if let Ok(tls) = self.acceptor.accept(tcp).await {
        return (tls, addr);
      }
    }
  }

  fn local_addr(&self) -> std::io::Result<Self::Addr> {
    self.tcp.local_addr()
  }
}

/// A throwaway certificate for `host` (self-signed: trusting it means adding it as a root).
fn cert(host: &str) -> NodeCertificate {
  NodeCertificate::self_signed(host).expect("certificate")
}

fn der(cert: &NodeCertificate) -> Vec<u8> {
  cert.chain()[0].to_vec()
}

/// The RPC server over TLS with `cert`: its port.
async fn serve_tls(cert: &NodeCertificate) -> u16 {
  let identity = Identity::from_certificate(cert).expect("identity");
  let mut tls = server_tls_config_with(Arc::new(identity), None).expect("server config");
  tls.alpn_protocols = vec![b"http/1.1".to_vec()];
  let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = tcp.local_addr().unwrap().port();
  let listener = TlsListener {
    tcp,
    acceptor: TlsAcceptor::from(Arc::new(tls)),
  };
  tokio::spawn(async move { axum::serve(listener, app()).await.unwrap() });
  port
}

/// The RPC server over plain TCP: its address.
async fn serve_plain() -> SocketAddr {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move { axum::serve(listener, app()).await.unwrap() });
  addr
}

fn wss(port: u16) -> String {
  format!("wss://localhost:{port}/rpc/ws")
}

/// The items of a call and its (successful, `null`) exit.
async fn items(call: &mut WsCall) -> Vec<Value> {
  let mut out = Vec::new();
  loop {
    match call.next().await {
      Some(CallEvent::Item { value, .. }) => out.push(value),
      Some(CallEvent::Exit(result)) => {
        assert_eq!(result.unwrap(), Value::Null);
        return out;
      }
      None => panic!("no exit"),
    }
  }
}

/// Every kind over one connection, whichever codec it negotiated.
async fn every_kind(client: &WsClient) {
  assert_eq!(
    client.call("Greeter.greet", json!("Ada")).await.unwrap(),
    json!("Hello, Ada!")
  );
  assert_eq!(
    client.call("Counter.add", json!(41)).await.unwrap(),
    json!(42)
  );
  let mut up = client.open("Count.up", json!(3), None).await.unwrap();
  assert_eq!(items(&mut up).await, [json!(1), json!(2), json!(3)]);
  assert_eq!(
    client
      .client_stream("Sum.all", [json!(1), json!(2), json!(4)])
      .await
      .unwrap(),
    json!(7)
  );
  let mut shout = client.open("Shout.back", Value::Null, None).await.unwrap();
  shout.send(json!("hi")).await.unwrap();
  shout.send(json!("there")).await.unwrap();
  shout.end().await.unwrap();
  assert_eq!(items(&mut shout).await, [json!("HI"), json!("THERE")]);
  let mut feed = client.open("Feed.watch", json!("e"), None).await.unwrap();
  let first = feed.next().await;
  assert!(
    matches!(&first, Some(CallEvent::Item { value, cursor: Some(_) }) if value == &json!("e1")),
    "{first:?}"
  );
  assert_eq!(items(&mut feed).await, [json!("e2")]);
  // A guard refusal is the call's `Rejected` exit, as over plain `ws://`.
  let refused = client.call("Secret.read", json!("x")).await.unwrap_err();
  assert_eq!(refused.failure_tag(), Some("Rejected"), "{refused:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_client_wss_round_trip() {
  let cert = cert("localhost");
  let port = serve_tls(&cert).await;
  let config = WsClientConfig::new().add_root_certificate_der(der(&cert));
  assert_eq!(config.root_certificates(), [der(&cert)]);
  let client = WsClient::connect_with(wss(port).as_str(), &config)
    .await
    .unwrap();
  assert_eq!(client.codec(), CodecId::Json);
  assert_eq!(client.hello().codec, "idrpc.json.v1");
  every_kind(&client).await;

  // A request carries the upgrade's headers: the guard reads them.
  let mut request = wss(port).into_client_request().unwrap();
  request
    .headers_mut()
    .insert("authorization", HeaderValue::from_static("Bearer ok"));
  let authorized = WsClient::connect_with(request, &config).await.unwrap();
  assert_eq!(
    authorized.call("Secret.read", json!("s3")).await.unwrap(),
    json!("s3")
  );

  // The generated clients' transport over the same socket.
  use id_effect_rpc::service::{RpcTransport as _, TransportCall};
  let call = TransportCall {
    tag: "Greeter.greet".to_owned(),
    version: 1,
    kind: id_effect_rpc::ProcedureKind::Query,
    payload: json!("Bo"),
  };
  let exit = authorized.unary(call).await.unwrap();
  assert_eq!(
    serde_json::to_value(exit).unwrap(),
    json!({"_tag": "Success", "value": "Hello, Bo!"})
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_client_rejects_untrusted_server_cert() {
  let cert = cert("localhost");
  let port = serve_tls(&cert).await;
  let refused = |result: Result<WsClient, RpcClientError>| match result {
    Err(RpcClientError::Transport(why)) => why,
    other => panic!("expected a TLS refusal, got {other:?}"),
  };

  // The platform roots do not hold a throwaway certificate.
  let why = refused(WsClient::connect(&wss(port)).await);
  assert!(why.to_lowercase().contains("certificate"), "{why}");

  // Trusting another certificate does not trust this one.
  let other = WsClientConfig::new().add_root_certificate_der(der(&self::cert("localhost")));
  let why = refused(WsClient::connect_with(wss(port).as_str(), &other).await);
  // (Both throwaway certificates carry rcgen's default subject, so the issuer matches and the
  // signature does not.)
  assert!(why.contains("invalid peer certificate"), "{why}");

  // A trusted certificate for another name is refused too (the server name is checked).
  let misnamed = self::cert("other.test");
  let port_misnamed = serve_tls(&misnamed).await;
  let trusting = WsClientConfig::new().add_root_certificate_der(der(&misnamed));
  let why = refused(WsClient::connect_with(wss(port_misnamed).as_str(), &trusting).await);
  assert!(why.contains("not valid for name"), "{why}");

  // A root that is not a certificate fails the connection; it never widens trust.
  let garbage = WsClientConfig::new().add_root_certificate_der(b"not a certificate".to_vec());
  let why = refused(WsClient::connect_with(wss(port).as_str(), &garbage).await);
  assert!(why.contains("wss trust roots"), "{why}");

  // The same server is reachable once its certificate is trusted.
  let trusted = WsClientConfig::new().add_root_certificate_der(der(&cert));
  let client = WsClient::connect_with(wss(port).as_str(), &trusted)
    .await
    .unwrap();
  assert_eq!(
    client.call("Counter.add", json!(1)).await.unwrap(),
    json!(2)
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_client_json_still_default() {
  let config = WsClientConfig::default();
  assert_eq!(config.codec(), CodecId::Json);
  assert!(config.root_certificates().is_empty());
  assert!(format!("{config:?}").contains("Json"), "{config:?}");

  let addr = serve_plain().await;
  let client = WsClient::connect(&format!("ws://{addr}/rpc/ws"))
    .await
    .unwrap();
  assert_eq!(client.codec(), CodecId::Json);
  assert_eq!(client.hello().codec, "idrpc.json.v1");
  every_kind(&client).await;
  // A procedure without typed codecs is fine over JSON.
  assert_eq!(
    client.call("Plain.echo", json!("j")).await.unwrap(),
    json!("j")
  );
  // Not a WebSocket URL.
  assert!(matches!(
    WsClient::connect(&format!("http://{addr}/rpc/ws")).await,
    Err(RpcClientError::Transport(_))
  ));
}

#[cfg(feature = "postcard")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_client_postcard_round_trip() {
  use futures_util::StreamExt as _;
  use id_effect_rpc::service::{RpcTransport, TransportCall, TransportError};

  let cert = cert("localhost");
  let port = serve_tls(&cert).await;
  let config = WsClientConfig::new()
    .add_root_certificate_der(der(&cert))
    .postcard(group());
  assert_eq!(config.codec(), CodecId::Postcard);
  let client = WsClient::connect_with(wss(port).as_str(), &config)
    .await
    .unwrap();
  assert_eq!(client.codec(), CodecId::Postcard);
  assert_eq!(client.hello().codec, "idrpc.postcard.v1");
  every_kind(&client).await;

  // Refused before anything is sent: a procedure the schemas lack (404), one without typed
  // codecs (415), an input its Schema does not decode.
  let unknown = client.call("Nope.none", json!(1)).await.unwrap_err();
  assert!(
    matches!(&unknown, RpcClientError::Refused(p) if p.status.as_u16() == 404),
    "{unknown:?}"
  );
  let untyped = client.call("Plain.echo", json!("x")).await.unwrap_err();
  assert!(
    matches!(&untyped, RpcClientError::Refused(p) if p.status.as_u16() == 415),
    "{untyped:?}"
  );
  let bad = client.call("Counter.add", json!("one")).await.unwrap_err();
  assert!(matches!(bad, RpcClientError::Transport(_)), "{bad:?}");
  // The connection is still good.
  assert_eq!(
    client.call("Counter.add", json!(9)).await.unwrap(),
    json!(10)
  );

  // The generated clients' transport: every kind, and the refusals as statuses.
  let call = |tag: &str, kind, payload| TransportCall {
    tag: tag.to_owned(),
    version: 1,
    kind,
    payload,
  };
  use id_effect_rpc::ProcedureKind as K;
  let exit = client
    .unary(call("Greeter.greet", K::Query, json!("Cy")))
    .await
    .unwrap();
  assert_eq!(
    serde_json::to_value(exit).unwrap(),
    json!({"_tag": "Success", "value": "Hello, Cy!"})
  );
  match client.unary(call("Nope.none", K::Query, json!(1))).await {
    Err(TransportError::Refused { status: 404, .. }) => {}
    other => panic!("{other:?}"),
  }
  let (tx, rx) = tokio::sync::mpsc::channel(4);
  for n in [5, 6] {
    tx.send(json!(n)).await.unwrap();
  }
  drop(tx);
  let exit =
    RpcTransport::client_stream(&client, call("Sum.all", K::ClientStream, Value::Null), rx)
      .await
      .unwrap();
  assert_eq!(
    serde_json::to_value(exit).unwrap(),
    json!({"_tag": "Success", "value": 11})
  );
  let (tx, rx) = tokio::sync::mpsc::channel(4);
  tx.send(json!("pc")).await.unwrap();
  drop(tx);
  let frames: Vec<_> = client
    .bidi(call("Shout.back", K::Bidi, Value::Null), rx)
    .await
    .unwrap()
    .collect()
    .await;
  assert_eq!(
    frames.first().and_then(|f| f.payload.clone()),
    Some(json!("PC"))
  );
  let frames: Vec<_> = client
    .server_stream(call("Count.up", K::ServerStream, json!(2)))
    .await
    .unwrap()
    .collect()
    .await;
  assert_eq!(frames.len(), 3, "two items and the exit: {frames:?}");

  // A socket opened by hand that negotiated postcard.
  let addr = serve_plain().await;
  let mut request = format!("ws://{addr}/rpc/ws").into_client_request().unwrap();
  request.headers_mut().insert(
    "sec-websocket-protocol",
    HeaderValue::from_static("idrpc.postcard.v1"),
  );
  let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
  let manual = WsClient::over_postcard(socket, group()).await.unwrap();
  assert_eq!(
    manual.call("Greeter.greet", json!("Di")).await.unwrap(),
    json!("Hello, Di!")
  );
  // A postcard client on a JSON socket never sees a postcard hello.
  let (socket, _) = tokio_tungstenite::connect_async({
    let mut request = format!("ws://{addr}/rpc/ws").into_client_request().unwrap();
    request.headers_mut().insert(
      "sec-websocket-protocol",
      HeaderValue::from_static("idrpc.json.v1"),
    );
    request
  })
  .await
  .unwrap();
  assert!(WsClient::over_postcard(socket, group()).await.is_err());
}
