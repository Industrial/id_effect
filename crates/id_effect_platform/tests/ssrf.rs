//! The SSRF guard and redirect policy of the reqwest client (M3, ADR 0013 R6): the default
//! client refuses loopback, private, link-local and metadata destinations (literal or resolved),
//! follows no redirect, and re-checks every hop when redirects are opted in.

mod common;

use std::net::IpAddr;
use std::time::Duration;

use common::{local_config, run};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::ssrf::{DANGEROUS_ALLOW_PRIVATE_NETWORKS, forbidden_range};
use id_effect_platform::http::{
  HttpClient, HttpClientConfig, HttpClientEnv, HttpRequest, HttpResponse, IpCidr,
  ReqwestHttpClient, ReqwestHttpClientProvider, SsrfPolicy, execute,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ip(s: &str) -> IpAddr {
  s.parse().unwrap()
}

async fn get(client: &ReqwestHttpClient, url: &str) -> Result<HttpResponse, HttpError> {
  let mut req = HttpRequest::get(url);
  req.timeout = Some(Duration::from_secs(5));
  run(client.execute(req)).await
}

/// `out` is a refusal by the SSRF policy whose message contains `needle`.
fn assert_blocked(out: Result<HttpResponse, HttpError>, needle: &str) {
  match out {
    Err(HttpError::InvalidRequest(m)) if m.contains("SSRF policy") && m.contains(needle) => {}
    other => panic!("expected an SSRF refusal naming {needle:?}, got {other:?}"),
  }
}

/// A mock server answering `GET /hello` with `200 ok`.
async fn hello_server() -> MockServer {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/hello"))
    .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
    .mount(&server)
    .await;
  server
}

fn port(server: &MockServer) -> u16 {
  server.address().port()
}

#[tokio::test]
async fn m3_ssrf_blocks_loopback_private_and_metadata() {
  let client = ReqwestHttpClient::default_client();
  for (url, range) in [
    ("http://127.0.0.1:1/", "loopback"),
    ("http://127.1.2.3/", "loopback"),
    ("http://2130706433/", "loopback"),
    ("http://0x7f.0.0.1/", "loopback"),
    ("http://[::1]/", "loopback"),
    ("http://0.0.0.0/", "unspecified"),
    ("http://[::]/", "unspecified"),
    ("http://10.0.0.1/", "RFC 1918"),
    ("http://172.16.0.1/", "RFC 1918"),
    ("http://172.31.255.255/", "RFC 1918"),
    ("https://192.168.1.1/", "RFC 1918"),
    ("http://100.64.0.1/", "carrier-grade NAT"),
    ("http://169.254.169.254/latest/meta-data/", "link-local"),
    ("http://[fe80::1]/", "link-local"),
    ("http://[fd00:ec2::254]/latest/meta-data/", "unique local"),
    ("http://[fc00::1]/", "unique local"),
    ("http://224.0.0.1/", "multicast"),
    ("http://[ff02::1]/", "multicast"),
    ("http://255.255.255.255/", "broadcast"),
  ] {
    assert_blocked(get(&client, url).await, range);
  }
  // Only http and https.
  assert_blocked(get(&client, "ftp://example.com/").await, "scheme `ftp`");
  assert_blocked(get(&client, "file:///etc/passwd").await, "scheme `file`");

  // A local server is refused before any request reaches it, through the default layer too.
  let server = hello_server().await;
  let url = format!("{}/hello", server.uri());
  assert_blocked(get(&client, &url).await, "127.0.0.1");
  let layered = run(
    execute::<HttpClientEnv, _>(HttpRequest::get(url))
      .provide_layer(ReqwestHttpClientProvider::layer()),
  )
  .await;
  assert_blocked(layered, "127.0.0.1");
  assert!(server.received_requests().await.unwrap().is_empty());

  // Public addresses are not in any range.
  for public in ["93.184.215.14", "8.8.8.8", "2606:4700::1111"] {
    assert_eq!(forbidden_range(ip(public)), None, "{public}");
  }
}

#[tokio::test]
async fn m3_ssrf_checks_every_resolved_address() {
  // One forbidden address among public ones refuses the name (a rebinding answer).
  let policy = SsrfPolicy::new();
  let err = policy
    .check_resolved(
      "rebind.example",
      [ip("93.184.215.14"), ip("169.254.169.254")],
    )
    .unwrap_err();
  assert!(err.reason().contains("169.254.169.254"), "{err}");
  assert!(
    policy
      .check_resolved(
        "rebind.example",
        [ip("93.184.215.14"), ip("2606:4700::1111")]
      )
      .is_ok()
  );

  // Through the client: a name is checked by its resolver, on the addresses it connects to.
  let server = hello_server().await;
  let by_name = format!("http://localhost:{}/hello", port(&server));
  assert_blocked(
    get(&ReqwestHttpClient::default_client(), &by_name).await,
    "localhost",
  );
  assert!(server.received_requests().await.unwrap().is_empty());

  // With every loopback address exempt by range, the resolved name connects.
  let loopback = HttpClientConfig::default().allow_cidrs([
    "127.0.0.0/8".parse::<IpCidr>().unwrap(),
    "::1".parse().unwrap(),
  ]);
  let res = get(&ReqwestHttpClient::from_config(&loopback), &by_name)
    .await
    .expect("allowed by range");
  assert_eq!((res.status, res.body.as_slice()), (200, &b"ok"[..]));
}

#[tokio::test]
async fn m3_ipv4_mapped_ipv6_is_blocked() {
  let client = ReqwestHttpClient::default_client();
  for url in [
    "http://[::ffff:127.0.0.1]/",
    "http://[::ffff:7f00:1]/",
    "http://[::ffff:169.254.169.254]/",
    "http://[::ffff:10.0.0.1]/",
    "http://[::ffff:8.8.8.8]/",
  ] {
    assert_blocked(get(&client, url).await, "IPv4-mapped");
  }
  assert_blocked(
    get(&client, "http://[::127.0.0.1]/").await,
    "IPv4-compatible",
  );
  assert_blocked(get(&client, "http://[64:ff9b::a9fe:a9fe]/").await, "NAT64");

  // An IPv4 exemption does not cover the mapped form.
  let v4 = HttpClientConfig::default().allow_cidrs(["127.0.0.0/8".parse::<IpCidr>().unwrap()]);
  assert_blocked(
    get(
      &ReqwestHttpClient::from_config(&v4),
      "http://[::ffff:127.0.0.1]:1/",
    )
    .await,
    "IPv4-mapped",
  );
}

/// A mock server: `GET /from` redirects (`302`) to `location`, `GET /to` answers `200 landed`.
async fn redirecting(location: impl Fn(&MockServer) -> String) -> MockServer {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/from"))
    .respond_with(ResponseTemplate::new(302).insert_header("Location", location(&server).as_str()))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/to"))
    .respond_with(ResponseTemplate::new(200).set_body_string("landed"))
    .mount(&server)
    .await;
  server
}

async fn hits(server: &MockServer, route: &str) -> usize {
  let received = server.received_requests().await.unwrap();
  received.iter().filter(|r| r.url.path() == route).count()
}

#[tokio::test]
async fn m3_redirects_not_followed_by_default() {
  let server = redirecting(|_| "/to".to_owned()).await;
  let from = format!("{}/from", server.uri());

  let res = get(&ReqwestHttpClient::from_config(&local_config()), &from)
    .await
    .expect("the 302 itself");
  assert_eq!(res.status, 302);
  assert!(
    res
      .headers
      .iter()
      .any(|(k, v)| k.eq_ignore_ascii_case("location") && v == "/to")
  );
  assert_eq!(hits(&server, "/to").await, 0);

  // Opted in, the same redirect is followed.
  let follow = ReqwestHttpClient::from_config(&local_config().follow_redirects(3));
  let res = get(&follow, &from).await.expect("followed");
  assert_eq!((res.status, res.body.as_slice()), (200, &b"landed"[..]));
  assert_eq!(hits(&server, "/to").await, 1);
}

#[tokio::test]
async fn m3_followed_redirect_to_private_ip_is_blocked() {
  let follow = ReqwestHttpClient::from_config(&local_config().follow_redirects(5));

  // A literal metadata address on the second hop.
  let metadata = redirecting(|_| "http://169.254.169.254/latest/meta-data/".to_owned()).await;
  assert_blocked(
    get(&follow, &format!("{}/from", metadata.uri())).await,
    "169.254.169.254",
  );

  // A host name on the second hop is checked by the resolver (only 127.0.0.1 is allowlisted).
  let named = redirecting(|s| format!("http://localhost:{}/to", s.address().port())).await;
  assert_blocked(
    get(&follow, &format!("{}/from", named.uri())).await,
    "localhost",
  );
  assert_eq!(hits(&named, "/to").await, 0);

  // A scheme change on a hop is refused too.
  let gopher = redirecting(|_| "gopher://example.com/".to_owned()).await;
  let out = get(&follow, &format!("{}/from", gopher.uri())).await;
  assert!(out.is_err(), "{out:?}");

  // And the hop count is capped.
  let looping = redirecting(|_| "/from".to_owned()).await;
  let capped = ReqwestHttpClient::from_config(&local_config().follow_redirects(2));
  match get(&capped, &format!("{}/from", looping.uri())).await {
    Err(HttpError::Reqwest(e)) => assert!(e.is_redirect(), "{e:?}"),
    other => panic!("expected a redirect error, got {other:?}"),
  }
  assert_eq!(hits(&looping, "/from").await, 3);
}

#[tokio::test]
async fn m3_allowlist_permits_named_host() {
  let server = hello_server().await;
  let by_name = format!("http://localhost:{}/hello", port(&server));

  // A listed name connects (matched case-insensitively, its addresses not checked).
  let named = HttpClientConfig::default().allow_hosts(["LOCALHOST"]);
  let res = get(&ReqwestHttpClient::from_config(&named), &by_name)
    .await
    .expect("allowlisted name");
  assert_eq!(res.status, 200);
  // Listing the name does not open its literal address.
  assert_blocked(
    get(
      &ReqwestHttpClient::from_config(&named),
      &format!("{}/hello", server.uri()),
    )
    .await,
    "127.0.0.1",
  );

  // The allowlist comes from config.
  let from_config: SsrfPolicy =
    serde_json::from_str(r#"{ "allow_hosts": ["localhost"] }"#).unwrap();
  let config = HttpClientConfig::default().with_ssrf(from_config);
  assert_eq!(
    get(&ReqwestHttpClient::from_config(&config), &by_name)
      .await
      .unwrap()
      .status,
    200
  );
  assert_eq!(hits(&server, "/hello").await, 2);
}

#[tokio::test]
async fn dangerous_allow_private_networks_is_recorded_and_opens_loopback() {
  let server = hello_server().await;
  let open = HttpClientConfig::default().dangerous_allow_private_networks();
  assert!(open.ssrf.allows_private_networks());
  let res = get(
    &ReqwestHttpClient::from_config(&open),
    &format!("{}/hello", server.uri()),
  )
  .await
  .expect("opted out");
  assert_eq!(res.status, 200);
  let recorded = id_effect::observability::posture::dangerous_opt_outs();
  assert!(
    recorded
      .iter()
      .any(|(id, reopens)| *id == DANGEROUS_ALLOW_PRIVATE_NETWORKS && reopens.contains("SSRF")),
    "{recorded:?}"
  );
}
