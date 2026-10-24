//! The edges: input the Relay refuses (typed failures, nothing stored), a mesh peer whose
//! certificate is not pinned, a port that is taken, and what nodes print about themselves (no
//! secret in any `Debug`).

mod common;

use std::sync::Arc;

use common::{Options, cluster, database, mesh_pki, node_config, run, run_typed};
use id_effect_kit::platform::tls::NodeCertificate;
use id_effect_kit::rpc::service::RpcCallError;
use id_effect_kit::sql::{SqlClient, TestSqlClient};
use relay::api::{Invite, JoinRoom, MAX_STATUS_LEN, Outgoing, RelayError};
use relay::node::RelayNode;
use relay::rooms::Rooms;

fn typed<A: std::fmt::Debug>(result: Result<A, RpcCallError<RelayError>>) -> RelayError {
  match result {
    Err(RpcCallError::Typed(e)) => e,
    other => panic!("expected a typed failure: {other:?}"),
  }
}

fn invalid_room(e: &RelayError) -> bool {
  matches!(e, RelayError::InvalidRoom { .. })
}

fn invalid_text(e: &RelayError) -> bool {
  matches!(e, RelayError::InvalidText { .. })
}

fn no_db() -> Arc<dyn SqlClient> {
  Arc::new(TestSqlClient::new())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_refuses_invalid_input() {
  let Some(db) = database("relay_refuses_invalid_input").await else {
    return;
  };
  let c = cluster(
    db,
    Options {
      nodes: 1,
      ..Options::default()
    },
  )
  .await;
  let ada = c.http(0, "ada").await;
  assert!(invalid_room(&typed(
    run_typed(ada.create_room("Bad Room".to_owned())).await
  )));
  run_typed(ada.create_room("general".to_owned()))
    .await
    .unwrap();
  let invite = |room: &str, user: &str| Invite {
    room: room.to_owned(),
    user: user.to_owned(),
  };
  assert!(invalid_room(&typed(
    run_typed(ada.invite(invite("", "bob"))).await
  )));
  assert!(invalid_text(&typed(
    run_typed(ada.invite(invite("general", " "))).await
  )));
  let post = |room: &str, text: &str| Outgoing {
    room: room.to_owned(),
    text: text.to_owned(),
  };
  assert!(invalid_room(&typed(
    run_typed(ada.send(post("UPPER", "hi"))).await
  )));
  assert!(invalid_text(&typed(
    run_typed(ada.send(post("general", "  "))).await
  )));
  assert!(invalid_text(&typed(
    run_typed(ada.send(post("general", &"x".repeat(2_001)))).await
  )));

  // The stream procedures check their input before anything is joined or read.
  let ws = c.ws(0, "ada").await;
  let join = |room: &str, status: &str| JoinRoom {
    room: room.to_owned(),
    status: status.to_owned(),
  };
  let first = |e| async move { run_typed(e).await };
  assert!(invalid_room(&typed(
    first(
      ws.join(join("a b", "online"), None)
        .flat_map(|s| s.take(1).run_collect())
    )
    .await
  )));
  assert!(invalid_text(&typed(
    first(
      ws.join(join("general", &"s".repeat(MAX_STATUS_LEN + 1)), None)
        .flat_map(|s| s.take(1).run_collect())
    )
    .await
  )));
  assert!(invalid_room(&typed(
    run_typed(
      ws.history("-".repeat(65), None)
        .flat_map(|s| s.take(1).run_collect())
    )
    .await
  )));
  // One connection is in a room once: a second join on the same socket is refused (busy).
  let (mut joined, _j) = common::pump(run(ws.join(join("general", "online"), None)).await.unwrap());
  assert_eq!(common::next(&mut joined).await.value.kind, "joined");
  assert_eq!(
    typed(
      run_typed(
        ws.join(join("general", "away"), None)
          .flat_map(|s| s.take(1).run_collect())
      )
      .await
    ),
    RelayError::Busy
  );
  // Nothing was stored: the first message of the room is number 1.
  let ack = run_typed(ada.send(post("general", "first"))).await.unwrap();
  assert_eq!(ack.seq, 1);
  c.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_refuses_a_mesh_peer_it_does_not_pin() {
  let certificate = NodeCertificate::self_signed(common::HOST).unwrap();
  let options = Options {
    nodes: 1,
    ..Options::default()
  };
  // Two meshes of one node each: neither pins the other's certificate.
  let a = RelayNode::start(node_config(
    0,
    &no_db(),
    &certificate,
    mesh_pki(1).remove(0),
    &options,
  ))
  .await
  .unwrap();
  let b = RelayNode::start(node_config(
    1,
    &no_db(),
    &certificate,
    mesh_pki(1).remove(0),
    &options,
  ))
  .await
  .unwrap();
  let refused = a
    .connect(b.mesh_addr())
    .await
    .expect_err("an unpinned peer");
  assert!(refused.0.starts_with("mesh"), "{refused}");
  assert!(a.connected_peers().is_empty());
  assert!(b.connected_peers().is_empty());
  assert!(b.state().peers().is_empty());
  a.shutdown().await;
  b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_refuses_a_taken_port() {
  let certificate = NodeCertificate::self_signed(common::HOST).unwrap();
  // TCP taken (the UDP twin is free): the node does not start.
  let tcp = std::net::TcpListener::bind((common::HOST, 0)).unwrap();
  let port = tcp.local_addr().unwrap().port();
  let options = Options {
    nodes: 1,
    port,
    ..Options::default()
  };
  let config = node_config(0, &no_db(), &certificate, mesh_pki(1).remove(0), &options);
  let refused = RelayNode::start(config).await.expect_err("TCP taken");
  assert!(refused.0.starts_with("tcp"), "{refused}");
  // UDP taken.
  let udp = std::net::UdpSocket::bind((common::HOST, 0)).unwrap();
  let options = Options {
    nodes: 1,
    port: udp.local_addr().unwrap().port(),
    ..Options::default()
  };
  let config = node_config(0, &no_db(), &certificate, mesh_pki(1).remove(0), &options);
  let refused = RelayNode::start(config).await.expect_err("UDP taken");
  assert!(refused.0.starts_with("quic"), "{refused}");
  assert!(refused.to_string().starts_with("relay: quic"));
  // The mesh's UDP port taken: the host it had started stops, the node does not start.
  let mut config = node_config(
    0,
    &no_db(),
    &certificate,
    mesh_pki(1).remove(0),
    &Options {
      nodes: 1,
      ..Options::default()
    },
  );
  config.mesh_bind = udp.local_addr().unwrap();
  let refused = RelayNode::start(config).await.expect_err("mesh UDP taken");
  assert!(refused.0.starts_with("mesh endpoint"), "{refused}");
}

/// The URL's `(scheme, host, port)`, the port defaulted by scheme.
fn origin_of(url: &str) -> (String, String, u16) {
  let (scheme, rest) = url.split_once("://").expect("an absolute URL");
  let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
  let (host, port) = match authority.rsplit_once(':') {
    Some((host, port)) => (host, port.parse().expect("a port")),
    None => (
      authority,
      if scheme == "http" || scheme == "ws" {
        80
      } else {
        443
      },
    ),
  };
  (scheme.to_owned(), host.to_owned(), port)
}

/// Whether `csp` (as served on a page of `page`) admits a connection to `url`: CSP Level 3
/// `connect-src` (else `default-src`) matching for `'self'` and `scheme://host[:port|:*]`
/// sources, which is all the Relay's policy holds. Scheme, host and port must all match.
fn csp_admits(csp: &str, page: &str, url: &str) -> bool {
  let directive = |name: &str| {
    csp
      .split(';')
      .map(str::trim)
      .find_map(|d| d.strip_prefix(name).filter(|rest| rest.starts_with(' ')))
      .map(str::to_owned)
  };
  let Some(sources) = directive("connect-src").or_else(|| directive("default-src")) else {
    return true;
  };
  let (page_scheme, page_host, page_port) = origin_of(page);
  let (scheme, host, port) = origin_of(url);
  sources.split_whitespace().any(|source| {
    if source == "'self'" {
      // `'self'` also admits the secure upgrade of the page's scheme (https → wss).
      let scheme_ok = scheme == page_scheme || (page_scheme == "https" && scheme == "wss");
      return scheme_ok && host == page_host && port == page_port;
    }
    if !source.contains("://") {
      return false;
    }
    let (s_scheme, rest) = source.split_once("://").unwrap();
    let (s_host, s_port) = match rest.rsplit_once(':') {
      Some((h, "*")) => (h, None),
      Some((h, p)) => (h, Some(p.parse::<u16>().unwrap())),
      None => (rest, Some(origin_of(source).2)),
    };
    s_scheme == scheme && s_host == host && s_port.is_none_or(|p| p == port)
  })
}

#[test]
fn the_csp_matcher_matches_scheme_host_and_port() {
  let csp = "default-src 'self'; connect-src 'self' https://lb.example:4433";
  let page = "https://127.0.0.1:8443/";
  assert!(csp_admits(csp, page, "https://127.0.0.1:8443/rpc/wt"));
  assert!(csp_admits(csp, page, "wss://127.0.0.1:8443/rpc/ws"));
  assert!(csp_admits(csp, page, "https://lb.example:4433/rpc/wt"));
  assert!(!csp_admits(csp, page, "https://127.0.0.1:8444/rpc/wt"));
  assert!(!csp_admits(csp, page, "https://localhost:8443/rpc/wt"));
  assert!(!csp_admits(csp, page, "http://127.0.0.1:8443/rpc/wt"));
  assert!(!csp_admits(csp, page, "https://lb.example/rpc/wt"));
  assert!(csp_admits(
    "default-src 'self'",
    page,
    "https://127.0.0.1:8443/x"
  ));
  assert!(csp_admits(
    "connect-src https://lb.example:*",
    page,
    "https://lb.example:1/"
  ));
  assert!(!csp_admits(
    "connect-src 'none'",
    page,
    "https://127.0.0.1:8443/"
  ));
  assert!(csp_admits("img-src 'none'", page, "https://a.example/"));
}

/// The page a node serves admits its own WebTransport URL (`https://host:port/rpc/wt`, the
/// page's own origin: TCP and UDP share the port) and the configured `connect_src` ones (a
/// WebTransport endpoint on another port), and nothing else: not another port, not the same
/// address spelt `localhost`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_csp_admits_its_webtransport_url() {
  use id_effect_kit::platform::http::{HttpClient as _, HttpRequest, ReqwestHttpClient};

  let certificate = NodeCertificate::self_signed(common::HOST).unwrap();
  let lb = format!("https://{}:4433", common::HOST);
  let options = Options {
    nodes: 1,
    connect_src: vec![lb.clone()],
    ..Options::default()
  };
  let config = node_config(0, &no_db(), &certificate, mesh_pki(1).remove(0), &options);
  let node = RelayNode::start(config).await.unwrap();
  let port = node.addr().port();
  let page = format!("https://{}:{port}/", common::HOST);
  let client = ReqwestHttpClient::from_config(&common::http_config(&certificate));
  let response = run(client.execute(HttpRequest::get(page.clone())))
    .await
    .unwrap();
  assert_eq!(response.status, 200);
  let csp = response
    .headers
    .iter()
    .find(|(name, _)| name.eq_ignore_ascii_case("content-security-policy"))
    .map(|(_, value)| value.clone())
    .expect("the page has a CSP");
  assert!(csp.contains("connect-src 'self' "), "{csp}");
  let host = common::HOST;
  for url in [
    format!("https://{host}:{port}/rpc/wt"),
    format!("wss://{host}:{port}/rpc/ws"),
    format!("{lb}/rpc/wt"),
  ] {
    assert!(csp_admits(&csp, &page, &url), "{url} refused by {csp}");
  }
  for url in [
    format!("https://{host}:{}/rpc/wt", port.wrapping_add(1)),
    format!("https://localhost:{port}/rpc/wt"),
  ] {
    assert!(!csp_admits(&csp, &page, &url), "{url} admitted by {csp}");
  }
  node.shutdown().await;

  // A source that cannot be written into the header: the node does not start.
  let options = Options {
    nodes: 1,
    connect_src: vec!["https://a.example; script-src *".to_owned()],
    ..Options::default()
  };
  let config = node_config(0, &no_db(), &certificate, mesh_pki(1).remove(0), &options);
  let refused = RelayNode::start(config)
    .await
    .expect_err("an injected source");
  assert!(refused.0.starts_with("connect_src"), "{refused}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_debug_output_names_no_secret() {
  let certificate = NodeCertificate::self_signed(common::HOST).unwrap();
  let options = Options {
    nodes: 1,
    ..Options::default()
  };
  let mesh = mesh_pki(1).remove(0);
  assert!(format!("{mesh:?}").contains("trusted: 1"));
  let config = node_config(0, &no_db(), &certificate, mesh, &options);
  let printed = format!("{config:?}");
  assert!(printed.contains("a@relay"), "{printed}");
  assert!(!printed.contains("cookie"), "{printed}");
  let node = RelayNode::start(config).await.unwrap();
  let printed = format!("{node:?}");
  assert!(printed.contains("a@relay"), "{printed}");
  assert!(format!("{:?}", node.state()).contains("a@relay"));
  assert_eq!(node.mesh().mesh().node_name(), "a@relay");
  assert_eq!(node.name(), "a@relay");
  let rooms = Rooms::new(no_db());
  assert!(format!("{rooms:?}").contains("Rooms"));
  assert!(Arc::ptr_eq(rooms.db(), rooms.db()));
  node.shutdown().await;
}
