//! The web client in Chromium (D12), driven from Rust over CDP (`headless_chrome`): the page
//! the node serves joins a room, posts over a two-way stream and sees a message posted on
//! another node — over WebTransport (the `id-effect dev` certificate: ECDSA, 13 days, pinned
//! with `serverCertificateHashes`), and with nothing answering on the WebTransport port, over
//! the RPC WebSocket. The first page failing over to WebSocket fails the test, and so does any
//! URL the page's CSP refuses (the second page's WebTransport port is in the node's
//! `connect_src`: it must fail on the network, not on the CSP).
//!
//! Runs only with `ID_EFFECT_REQUIRE_BROWSER=1` (then a missing Chromium fails the test, it never
//! skips); without it the test returns at once. It also needs Docker (PostgreSQL).

mod common;

use std::time::{Duration, Instant};

use common::{Options, cluster, database, run_typed};
use headless_chrome::{Browser, LaunchOptions};
use id_effect_kit::platform::tls::NodeCertificate;
use relay::api::{Invite, Outgoing};
use serde_json::Value;

const REQUIRE: &str = "ID_EFFECT_REQUIRE_BROWSER";

/// The pages' origin: the node itself, on whichever port it got.
const ORIGIN: &str = "https://127.0.0.1:*";

/// What one page should do and what it reported.
struct Page {
  url: String,
  expect: String,
}

fn percent(text: &str) -> String {
  text
    .bytes()
    .map(|b| match b {
      b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
        char::from(b).to_string()
      }
      _ => format!("%{b:02X}"),
    })
    .collect()
}

/// Opens `page` and returns `window.__relay` once its state is `state` (or `failed`), polling
/// for at most a minute.
fn wait_for(tab: &headless_chrome::Tab, state: &str) -> Value {
  let deadline = Instant::now() + Duration::from_secs(60);
  loop {
    let reported = tab
      .evaluate("JSON.stringify(window.__relay || null)", false)
      .ok()
      .and_then(|r| r.value)
      .and_then(|v| v.as_str().map(str::to_owned))
      .and_then(|s| serde_json::from_str::<Value>(&s).ok())
      .unwrap_or(Value::Null);
    if reported["state"] == state || reported["state"] == "failed" {
      return reported;
    }
    assert!(
      Instant::now() < deadline,
      "the page never reached {state}: {reported}"
    );
    std::thread::sleep(Duration::from_millis(100));
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_browser_chat_round_trip() {
  if std::env::var(REQUIRE).as_deref() != Ok("1") {
    eprintln!("browser test not run: set {REQUIRE}=1 (Chromium on PATH)");
    return;
  }
  let Some(db) = database("relay_browser_chat_round_trip").await else {
    return;
  };
  let dir = std::env::temp_dir().join(format!("relay-browser-cert-{}", std::process::id()));
  let dev = id_effect_tool::dev::cert::ensure(&dir, std::time::SystemTime::now()).unwrap();
  let certificate = NodeCertificate::from_pem_files(&dev.cert, &dev.key).unwrap();
  // Nothing answers on this port: the second page's WebTransport URL. The pages' CSP admits it
  // (CSP matches scheme, host and port), so the attempt fails on the network, not on the CSP.
  let dead_udp = std::net::UdpSocket::bind((common::HOST, 0)).unwrap();
  let dead_wt = format!(
    "https://{}:{}",
    common::HOST,
    dead_udp.local_addr().unwrap().port()
  );
  let c = cluster(
    db,
    Options {
      nodes: 2,
      certificate: Some(certificate),
      origins: vec![ORIGIN.to_owned()],
      connect_src: vec![dead_wt.clone()],
      ..Options::default()
    },
  )
  .await;
  let ada = c.http(0, "ada").await;
  run_typed(ada.create_room("general".to_owned()))
    .await
    .unwrap();
  run_typed(ada.invite(Invite {
    room: "general".to_owned(),
    user: "bob".to_owned(),
  }))
  .await
  .unwrap();
  let bob = c.http(1, "bob").await;

  let token = c.token("ada").await;
  let page = |text: &str, expect: &str, wt: Option<String>| {
    let mut url = format!(
      "{}/#token={token}&room=general&text={}&expect={}&hash={}",
      c.url(0),
      percent(text),
      percent(expect),
      dev.sha256_hex()
    );
    if let Some(wt) = wt {
      url.push_str(&format!("&wt={}", percent(&wt)));
    }
    Page {
      url,
      expect: expect.to_owned(),
    }
  };
  let pages = vec![
    page("from chromium", "from bob", None),
    page(
      "from chromium again",
      "from bob again",
      Some(format!("{dead_wt}/rpc/wt")),
    ),
  ];

  let options = LaunchOptions::default_builder()
    .headless(true)
    .sandbox(false)
    .ignore_certificate_errors(true)
    .idle_browser_timeout(Duration::from_secs(180))
    .build()
    .unwrap();
  let browser = Browser::new(options).expect("Chromium on PATH (ID_EFFECT_REQUIRE_BROWSER=1)");
  let mut reports = Vec::new();
  for page in pages {
    let tab = browser.new_tab().unwrap();
    let url = page.url.clone();
    let tab = tokio::task::spawn_blocking(move || {
      // Chromium aborts a load with ERR_NETWORK_CHANGED when the host's interfaces change
      // (the CI runner starts containers beside this test); load again, a bounded number of times.
      let mut attempt = 0;
      loop {
        attempt += 1;
        match tab
          .navigate_to(&url)
          .and_then(|tab| tab.wait_until_navigated())
        {
          Ok(_) => break,
          Err(e) if attempt < 3 && e.to_string().contains("ERR_NETWORK_CHANGED") => {
            std::thread::sleep(Duration::from_millis(500));
          }
          Err(e) => panic!("navigating to {url}: {e}"),
        }
      }
      let joined = wait_for(&tab, "joined");
      assert_eq!(joined["state"], "joined", "{joined}");
      tab
    })
    .await
    .unwrap();
    // The page is in the room: bob posts from the other node.
    run_typed(bob.send(Outgoing {
      room: "general".to_owned(),
      text: page.expect.clone(),
    }))
    .await
    .unwrap();
    let report = tokio::task::spawn_blocking(move || wait_for(&tab, "done"))
      .await
      .unwrap();
    reports.push(report);
  }

  // First page: WebTransport; second: WebSocket after the WebTransport attempt failed.
  for (report, transport) in reports.iter().zip(["webtransport", "websocket"]) {
    assert_eq!(report["state"], "done", "{report}");
    assert_eq!(report["transport"], transport, "{report}");
    assert_eq!(report["ack"]["room"], "general", "{report}");
    let froms: Vec<(&str, &str)> = report["messages"]
      .as_array()
      .unwrap()
      .iter()
      .map(|m| (m["from"].as_str().unwrap(), m["node"].as_str().unwrap()))
      .collect();
    assert!(froms.contains(&("ada", "a@relay")), "{report}");
    assert!(froms.contains(&("bob", "b@relay")), "{report}");
  }
  // The first page must not have fallen back: any WebTransport failure there fails the test.
  assert!(
    reports[0]["why"].is_null(),
    "the first page fell back to WebSocket: {}",
    reports[0]
  );
  assert!(
    reports[1]["why"].as_str().is_some(),
    "the second page tried WebTransport first: {}",
    reports[1]
  );
  // It fell back because nothing answered, not because the page's CSP refused the URL.
  for report in &reports {
    let why = report["why"].as_str().unwrap_or_default();
    assert!(
      !why.contains("Content Security Policy"),
      "the CSP refused the WebTransport URL: {report}"
    );
    assert_eq!(
      report["violations"],
      serde_json::json!([]),
      "the CSP refused a URL: {report}"
    );
  }
  drop(dead_udp);
  c.shutdown().await;
  let _ = std::fs::remove_dir_all(dir);
}
