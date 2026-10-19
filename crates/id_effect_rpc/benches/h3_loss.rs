//! D9 bench (r12-e12-5-benches): p99 latency of 64 multiplexed 16 KiB responses over one
//! connection, HTTP/2 over TLS vs HTTP/3, under 1 % packet loss and 5 ms one-way delay.
//!
//! The server is the shipped host: [`ServerHandle`] with TLS serves the same axum router over
//! HTTP/2 (TCP) and HTTP/3 (UDP, the same port number, the platform `h3` route with its default
//! BBR congestion control). The clients are hyper's HTTP/2 client over tokio-rustls and `h3` over
//! the platform's HTTP/3 client config (BBR, 4 MiB socket buffers), each on one connection.
//! Every round sends 64 `GET`s at once and waits for all 64 bodies; a sample is one request's
//! latency, so a round contributes 64 samples. Trials alternate h2, h3 on fresh connections.
//!
//! Two ways to make the network lossy:
//!
//! - **netem** (`H3_LOSS_NETEM=1`; `.github/workflows/bench.yml`): the job runs
//!   `sudo tc qdisc add dev lo root netem loss 1% delay 5ms` around this bench, so both
//!   protocols go straight to the server through the kernel, each direction delayed and lossy.
//! - **userspace proxies** (the default; no `sudo` needed): a UDP proxy in front of HTTP/3 drops
//!   every datagram with probability `H3_LOSS_LOSS` and delays the rest by `H3_LOSS_DELAY_MS`,
//!   per direction; a TCP proxy in front of HTTP/2 cuts the byte stream into 1448-byte segments,
//!   delays each by the same amount and models a lost segment as a fast retransmit
//!   (`H3_LOSS_PENALTY_MS` later, every later byte waiting behind it, the congestion window cut
//!   by 0.7 once per RTT). That TCP model is generous to TCP (no RTO, ACK loss free); it is the
//!   model of the D9 spike (F22) and only stands in for netem where `sudo` is unavailable.
//!
//! Env: `H3_LOSS_ROUNDS` (200), `H3_LOSS_WARMUP` (10), `H3_LOSS_TRIALS` (3), `H3_LOSS_LOSS`
//! (0.01), `H3_LOSS_DELAY_MS` (5), `H3_LOSS_PENALTY_MS` (12.5), `H3_LOSS_NETEM`.
//!
//! Results: the median over trials of each trial's p99 (and p50), in ns, saved as criterion
//! estimates under `<target>/criterion/h3_loss/{h2_p99,h3_p99,h2_p50,h3_p50}/<baseline>/`
//! (`--save-baseline NAME`, as `scripts/bench-run.sh` passes it), so `scripts/bench-export.sh`
//! exports them as `h3_loss/*` and the pair gate `h3_loss/h3_p99 / h3_loss/h2_p99 ≤ 1.00`
//! (`docs/benchmarks/gates.toml`) applies; every trial goes to `summary-<baseline>.json` beside
//! them. Without `--bench` (`cargo test --benches`) or with `--test` it runs one short smoke
//! trial and saves nothing.
//!
//! Run: `cargo bench -p id_effect_rpc --features http3 --bench h3_loss -- --save-baseline head`

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use bytes::Buf as _;
use http_body_util::{BodyExt, Empty};
use id_effect_axum::server::HostTls;
use id_effect_axum::server::tls::ServerTls;
use id_effect_axum::{ConnectionLimits, HostConfig, ReadinessState, ServerHandle};
use id_effect_platform::quic::{ALPN_H3, CongestionControl, QuicLimits, client_config_with};
use id_effect_platform::tls::{Identity, NodeCertificate, provider};
use rand::Rng as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::rustls::{self, RootCertStore};

const BODY: usize = 16 * 1024;
const STREAMS: usize = 64;
const MSS: usize = 1448;

#[derive(Clone, Copy, Debug)]
struct Net {
  loss: f64,
  delay: Duration,
  penalty: Duration,
}

fn env<T: std::str::FromStr>(name: &str, default: T) -> T {
  std::env::var(name)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

// ---------------------------------------------------------------------------------------------
// The server: the shipped host, TLS on TCP (h2) and HTTP/3 on UDP
// ---------------------------------------------------------------------------------------------

async fn server(cert: &NodeCertificate) -> ServerHandle {
  let identity = Identity::from_certificate(cert).expect("identity");
  let tls = ServerTls::from_identity(&identity).expect("server tls");
  let config = HostConfig {
    bind_host: "127.0.0.1".into(),
    bind_port: 0,
    ..HostConfig::default()
  }
  .with_tls(HostTls::provided(tls));
  let body = Bytes::from(vec![7u8; BODY]);
  let app = axum::Router::new().route(
    "/x",
    axum::routing::get(move || {
      let body = body.clone();
      async move { body }
    }),
  );
  // One connection carries every round of a trial: lift the per-connection request cap (a
  // bound, not an opt-out) above rounds × 64.
  let limits = ConnectionLimits::default().max_requests_per_connection(10_000_000);
  ServerHandle::start_with_limits(&config, app, ReadinessState::new(true), limits)
    .await
    .expect("host")
}

fn client_tls(root: &CertificateDer<'static>, alpn: &[u8]) -> rustls::ClientConfig {
  let mut roots = RootCertStore::empty();
  roots.add(root.clone()).expect("root");
  let mut tls = rustls::ClientConfig::builder_with_provider(provider())
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("tls 1.3")
    .with_root_certificates(roots)
    .with_no_client_auth();
  tls.alpn_protocols = vec![alpn.to_vec()];
  tls
}

// ---------------------------------------------------------------------------------------------
// Userspace loss proxies (the D9 spike's, r12-e12-3-h3-server)
// ---------------------------------------------------------------------------------------------

fn lost(net: &Net) -> bool {
  net.loss > 0.0 && rand::rng().random_bool(net.loss)
}

/// A loopback UDP socket with 4 MiB buffers, so the proxy itself drops nothing.
fn big_udp() -> UdpSocket {
  let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).expect("udp");
  let _ = s.set_recv_buffer_size(4 << 20);
  let _ = s.set_send_buffer_size(4 << 20);
  s.set_nonblocking(true).expect("nonblocking");
  s.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())
    .expect("bind");
  UdpSocket::from_std(s.into()).expect("tokio udp")
}

/// UDP receive-buffer drops so far, machine-wide (`/proc/net/snmp` `RcvbufErrors`; 0 elsewhere).
fn rcvbuf_errors() -> u64 {
  let snmp = std::fs::read_to_string("/proc/net/snmp").unwrap_or_default();
  let mut udp = snmp.lines().filter(|l| l.starts_with("Udp: "));
  let (Some(names), Some(values)) = (udp.next(), udp.next()) else {
    return 0;
  };
  names
    .split_whitespace()
    .zip(values.split_whitespace())
    .find(|(n, _)| *n == "RcvbufErrors")
    .and_then(|(_, v)| v.parse().ok())
    .unwrap_or(0)
}

/// In-order delayed delivery: `(deadline, payload)` through `send`.
fn delayed<F, Fut>(send: F) -> mpsc::UnboundedSender<(Instant, Vec<u8>)>
where
  F: Fn(Vec<u8>) -> Fut + Send + 'static,
  Fut: std::future::Future<Output = bool> + Send,
{
  let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
  tokio::spawn(async move {
    while let Some((at, p)) = rx.recv().await {
      if at > Instant::now() {
        tokio::time::sleep_until(at).await;
      }
      if !send(p).await {
        break;
      }
    }
  });
  tx
}

async fn udp_proxy(upstream: SocketAddr, net: Net) -> SocketAddr {
  let front = Arc::new(big_udp());
  let back = Arc::new(big_udp());
  back.connect(upstream).await.expect("connect upstream");
  let addr = front.local_addr().expect("addr");
  let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
  let b = Arc::clone(&back);
  let up = delayed(move |p| {
    let b = Arc::clone(&b);
    async move { b.send(&p).await.is_ok() }
  });
  let (f, c) = (Arc::clone(&front), Arc::clone(&client));
  let down = delayed(move |p| {
    let (f, c) = (Arc::clone(&f), Arc::clone(&c));
    async move {
      let to = *c.lock().expect("client addr");
      match to {
        Some(to) => f.send_to(&p, to).await.is_ok(),
        None => true,
      }
    }
  });
  let (f, c) = (Arc::clone(&front), Arc::clone(&client));
  tokio::spawn(async move {
    let mut buf = vec![0u8; 65_536];
    while let Ok((n, from)) = f.recv_from(&mut buf).await {
      *c.lock().expect("client addr") = Some(from);
      if !lost(&net) {
        let _ = up.send((Instant::now() + net.delay, buf[..n].to_vec()));
      }
    }
  });
  tokio::spawn(async move {
    let mut buf = vec![0u8; 65_536];
    while let Ok(n) = back.recv(&mut buf).await {
      if !lost(&net) {
        let _ = down.send((Instant::now() + net.delay, buf[..n].to_vec()));
      }
    }
  });
  addr
}

/// One direction of the TCP proxy: a TCP sender with SACK on a path of RTT = 2 × delay
/// (congestion window from 10 segments, slow start, +1/cwnd per ACK in avoidance, × 0.7 once per
/// RTT on loss), each segment's ACK one delay after it arrives, a lost segment arriving `penalty`
/// later and holding every later byte behind it (in-order delivery).
async fn pipe(mut r: tokio::net::tcp::OwnedReadHalf, w: tokio::net::tcp::OwnedWriteHalf, net: Net) {
  let w = Arc::new(tokio::sync::Mutex::new(w));
  let tx = delayed(move |p| {
    let w = Arc::clone(&w);
    async move { w.lock().await.write_all(&p).await.is_ok() }
  });
  let (seg_tx, mut seg_rx) = mpsc::unbounded_channel::<Vec<u8>>();
  tokio::spawn(async move {
    let mut buf = vec![0u8; 65_536];
    loop {
      let n = match r.read(&mut buf).await {
        Ok(0) | Err(_) => break,
        Ok(n) => n,
      };
      for seg in buf[..n].chunks(MSS) {
        if seg_tx.send(seg.to_vec()).is_err() {
          return;
        }
      }
    }
  });
  let rtt = net.delay * 2;
  let mut cwnd: f64 = 10.0;
  let mut ssthresh = f64::INFINITY;
  let mut inflight = 0usize;
  let mut acks: std::collections::BinaryHeap<std::cmp::Reverse<Instant>> = Default::default();
  let mut queue: std::collections::VecDeque<Vec<u8>> = Default::default();
  let mut last = Instant::now();
  let mut recovery_end = Instant::now();
  let mut open = true;
  loop {
    let now = Instant::now();
    while let Some(std::cmp::Reverse(t)) = acks.peek() {
      if *t > now {
        break;
      }
      acks.pop();
      inflight -= 1;
      if cwnd < ssthresh {
        cwnd += 1.0;
      } else {
        cwnd += 1.0 / cwnd;
      }
    }
    while inflight < cwnd as usize {
      let Some(seg) = queue.pop_front() else { break };
      let mut at = now + net.delay;
      if lost(&net) {
        at += net.penalty;
        if now >= recovery_end {
          cwnd = (cwnd * 0.7).max(2.0);
          ssthresh = cwnd;
          recovery_end = now + rtt;
        }
      }
      acks.push(std::cmp::Reverse(at + net.delay));
      inflight += 1;
      let deliver = at.max(last);
      last = deliver;
      if tx.send((deliver, seg)).is_err() {
        return;
      }
    }
    if !open && queue.is_empty() {
      break;
    }
    let next_ack = acks.peek().map(|r| r.0);
    let blocked = !queue.is_empty() && next_ack.is_some();
    tokio::select! {
      seg = seg_rx.recv(), if open => match seg {
        Some(s) => queue.push_back(s),
        None => open = false,
      },
      () = tokio::time::sleep_until(next_ack.unwrap_or(now)), if blocked => {}
    }
  }
}

async fn tcp_proxy(upstream: SocketAddr, net: Net) -> SocketAddr {
  let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
  let addr = listener.local_addr().expect("addr");
  tokio::spawn(async move {
    while let Ok((c, _)) = listener.accept().await {
      let Ok(s) = TcpStream::connect(upstream).await else {
        continue;
      };
      let _ = c.set_nodelay(true);
      let _ = s.set_nodelay(true);
      let (cr, cw) = c.into_split();
      let (sr, sw) = s.into_split();
      tokio::spawn(pipe(cr, sw, net));
      tokio::spawn(pipe(sr, cw, net));
    }
  });
  addr
}

// ---------------------------------------------------------------------------------------------
// The clients
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Sample {
  per_request: Vec<Duration>,
}

async fn run_h2(
  addr: SocketAddr,
  root: &CertificateDer<'static>,
  rounds: usize,
  warmup: usize,
) -> Sample {
  let tcp = TcpStream::connect(addr).await.expect("connect");
  tcp.set_nodelay(true).expect("nodelay");
  let tls = tokio_rustls::TlsConnector::from(Arc::new(client_tls(root, b"h2")))
    .connect(ServerName::try_from("localhost").expect("name"), tcp)
    .await
    .expect("tls");
  let (send, conn) = hyper::client::conn::http2::handshake(
    hyper_util::rt::TokioExecutor::new(),
    hyper_util::rt::TokioIo::new(tls),
  )
  .await
  .expect("h2 handshake");
  tokio::spawn(conn);
  let mut sample = Sample::default();
  for round in 0..warmup + rounds {
    let mut set = JoinSet::new();
    for _ in 0..STREAMS {
      let mut s = send.clone();
      set.spawn(async move {
        let t = Instant::now();
        let req = http::Request::get("https://localhost/x")
          .body(Empty::<Bytes>::new())
          .expect("request");
        let res = s.send_request(req).await.expect("h2 response");
        assert_eq!(res.status(), 200);
        let body = res.into_body().collect().await.expect("body").to_bytes();
        assert_eq!(body.len(), BODY);
        t.elapsed()
      });
    }
    let mut lat = Vec::with_capacity(STREAMS);
    while let Some(d) = set.join_next().await {
      lat.push(d.expect("request task"));
    }
    if round >= warmup {
      sample.per_request.extend(lat);
    }
  }
  sample
}

async fn run_h3(
  addr: SocketAddr,
  root: &CertificateDer<'static>,
  rounds: usize,
  warmup: usize,
) -> Sample {
  // The shipped client side: the platform HTTP/3 client's config (BBR, default limits) on a
  // socket with the platform's 4 MiB buffers.
  let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).expect("udp");
  let _ = s.set_recv_buffer_size(4 << 20);
  let _ = s.set_send_buffer_size(4 << 20);
  s.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())
    .expect("bind");
  let ep = quinn::Endpoint::new(
    quinn::EndpointConfig::default(),
    None,
    s.into(),
    quinn::default_runtime().expect("runtime"),
  )
  .expect("endpoint");
  let config = client_config_with(
    client_tls(root, ALPN_H3),
    &[ALPN_H3],
    &QuicLimits::default(),
    CongestionControl::Bbr,
  )
  .expect("quic client config");
  let conn = ep
    .connect_with(config, addr, "localhost")
    .expect("connect")
    .await
    .expect("quic handshake");
  let (mut driver, send) = h3::client::builder()
    .build::<_, _, Bytes>(h3_quinn::Connection::new(conn))
    .await
    .expect("h3 handshake");
  tokio::spawn(async move {
    let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
  });
  let mut sample = Sample::default();
  for round in 0..warmup + rounds {
    let mut set = JoinSet::new();
    for _ in 0..STREAMS {
      let mut s = send.clone();
      set.spawn(async move {
        let t = Instant::now();
        let req = http::Request::get("https://localhost/x")
          .body(())
          .expect("request");
        let mut st = s.send_request(req).await.expect("h3 request");
        st.finish().await.expect("finish");
        let res = st.recv_response().await.expect("h3 response");
        assert_eq!(res.status(), 200);
        let mut n = 0;
        while let Some(mut chunk) = st.recv_data().await.expect("h3 body") {
          n += chunk.remaining();
          chunk.advance(chunk.remaining());
        }
        assert_eq!(n, BODY);
        t.elapsed()
      });
    }
    let mut lat = Vec::with_capacity(STREAMS);
    while let Some(d) = set.join_next().await {
      lat.push(d.expect("request task"));
    }
    if round >= warmup {
      sample.per_request.extend(lat);
    }
  }
  ep.close(0u32.into(), b"done");
  sample
}

// ---------------------------------------------------------------------------------------------
// Statistics and the criterion-format output
// ---------------------------------------------------------------------------------------------

/// The `p` quantile (nearest rank) of `sorted`, in ns.
fn pct(sorted: &[Duration], p: f64) -> f64 {
  let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
  sorted[i].as_nanos() as f64
}

/// One trial: (p50, p99) in ns.
fn summarize(proto: &str, trial: usize, mut s: Sample) -> (f64, f64) {
  s.per_request.sort();
  let (p50, p99) = (pct(&s.per_request, 0.50), pct(&s.per_request, 0.99));
  println!(
    "h3_loss: {proto} trial {trial}: {} samples, p50 {:.2} ms, p90 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
    s.per_request.len(),
    p50 / 1e6,
    pct(&s.per_request, 0.90) / 1e6,
    p99 / 1e6,
    pct(&s.per_request, 1.0) / 1e6,
  );
  (p50, p99)
}

fn median(v: &[f64]) -> f64 {
  let mut v = v.to_vec();
  v.sort_by(f64::total_cmp);
  v[v.len() / 2]
}

fn std_dev(v: &[f64]) -> f64 {
  let mean = v.iter().sum::<f64>() / v.len() as f64;
  (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
}

/// `<target>/criterion`: `CARGO_TARGET_DIR`, else three levels above this executable
/// (`<target>/release/deps/h3_loss-*`).
fn criterion_dir() -> PathBuf {
  let target = std::env::var_os("CARGO_TARGET_DIR")
    .map(PathBuf::from)
    .or_else(|| {
      let exe = std::env::current_exe().ok()?;
      Some(exe.parent()?.parent()?.parent()?.to_path_buf())
    })
    .unwrap_or_else(|| PathBuf::from("target"));
  target.join("criterion")
}

/// Saves the trials of `name` as criterion estimates of `h3_loss/<name>` under `baseline`.
fn save(dir: &std::path::Path, baseline: &str, name: &str, trials: &[f64]) {
  let at = dir.join("h3_loss").join(name).join(baseline);
  std::fs::create_dir_all(&at).expect("criterion dir");
  let full_id = format!("h3_loss/{name}");
  let benchmark = serde_json::json!({
    "group_id": "h3_loss", "function_id": name, "value_str": null, "throughput": null,
    "full_id": full_id, "directory_name": format!("h3_loss/{name}"), "title": full_id,
  });
  let lo = trials.iter().copied().fold(f64::INFINITY, f64::min);
  let hi = trials.iter().copied().fold(f64::NEG_INFINITY, f64::max);
  let estimate = |point: f64| {
    serde_json::json!({
      "confidence_interval": {"confidence_level": 0.95, "lower_bound": lo, "upper_bound": hi},
      "point_estimate": point, "standard_error": 0.0,
    })
  };
  let mean = trials.iter().sum::<f64>() / trials.len() as f64;
  let estimates = serde_json::json!({
    "mean": estimate(mean), "median": estimate(median(trials)),
    "median_abs_dev": estimate(0.0), "slope": null, "std_dev": estimate(std_dev(trials)),
  });
  std::fs::write(at.join("benchmark.json"), benchmark.to_string()).expect("benchmark.json");
  std::fs::write(at.join("estimates.json"), estimates.to_string()).expect("estimates.json");
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
  let args: Vec<String> = std::env::args().collect();
  // `cargo bench` passes `--bench`; `cargo test --benches` does not, and criterion's `--test`
  // asks for a smoke run as well.
  let bench = args.iter().any(|a| a == "--bench") && !args.iter().any(|a| a == "--test");
  let baseline = args
    .iter()
    .position(|a| a == "--save-baseline")
    .and_then(|i| args.get(i + 1))
    .cloned()
    .unwrap_or_else(|| "h3_loss".to_owned());
  let net = Net {
    loss: env("H3_LOSS_LOSS", 0.01),
    delay: Duration::from_secs_f64(env("H3_LOSS_DELAY_MS", 5.0) / 1000.0),
    penalty: Duration::from_secs_f64(env("H3_LOSS_PENALTY_MS", 12.5) / 1000.0),
  };
  let netem = std::env::var_os("H3_LOSS_NETEM").is_some_and(|v| v != "0");
  let (rounds, warmup, trials) = if bench {
    (
      env("H3_LOSS_ROUNDS", 200usize),
      env("H3_LOSS_WARMUP", 10usize),
      env("H3_LOSS_TRIALS", 3usize).max(1),
    )
  } else {
    (2, 0, 1)
  };
  let mode = if netem { "netem" } else { "userspace-proxy" };
  println!(
    "h3_loss: mode {mode}, {net:?}, {rounds} rounds (+{warmup} warm-up) x {STREAMS} streams x \
     {BODY} B, {trials} trials, baseline '{baseline}'"
  );
  let cert = NodeCertificate::self_signed("localhost").expect("certificate");
  let root = cert.chain()[0].clone();
  let host = server(&cert).await;
  let addr = host.local_addr();
  let (mut h2, mut h3, mut h2_50, mut h3_50, mut drops) = (vec![], vec![], vec![], vec![], vec![]);
  for trial in 0..trials {
    let front = if netem {
      addr
    } else {
      tcp_proxy(addr, net).await
    };
    let (p50, p99) = summarize("h2", trial, run_h2(front, &root, rounds, warmup).await);
    h2.push(p99);
    h2_50.push(p50);
    let before = rcvbuf_errors();
    let front = if netem {
      addr
    } else {
      udp_proxy(addr, net).await
    };
    let (p50, p99) = summarize("h3", trial, run_h3(front, &root, rounds, warmup).await);
    h3.push(p99);
    h3_50.push(p50);
    drops.push(rcvbuf_errors().saturating_sub(before));
  }
  host.shutdown().await;
  let ratio = median(&h3) / median(&h2);
  println!(
    "h3_loss: median p99 h2 {:.2} ms, h3 {:.2} ms, h3/h2 {ratio:.3} (gate <= 1.00, noise 0.05); \
     UDP receive-buffer drops per h3 trial {drops:?}",
    median(&h2) / 1e6,
    median(&h3) / 1e6,
  );
  if !bench {
    return;
  }
  let dir = criterion_dir();
  for (name, values) in [
    ("h2_p99", &h2),
    ("h3_p99", &h3),
    ("h2_p50", &h2_50),
    ("h3_p50", &h3_50),
  ] {
    save(&dir, &baseline, name, values);
  }
  let summary = serde_json::json!({
    "mode": mode, "loss": net.loss, "delay_ms": net.delay.as_secs_f64() * 1e3,
    "tcp_penalty_ms": if netem { None } else { Some(net.penalty.as_secs_f64() * 1e3) },
    "rounds": rounds, "warmup": warmup, "streams": STREAMS, "body_bytes": BODY,
    "h3_congestion_control": "bbr (Route::h3 default)",
    "trials_ns": {"h2_p99": h2, "h3_p99": h3, "h2_p50": h2_50, "h3_p50": h3_50},
    "udp_rcvbuf_drops": drops, "ratio_h3_over_h2_p99": ratio,
  });
  let path = dir.join("h3_loss").join(format!("summary-{baseline}.json"));
  std::fs::write(&path, serde_json::to_string_pretty(&summary).expect("json")).expect("summary");
  println!(
    "h3_loss: saved baseline '{baseline}' and {}",
    path.display()
  );
}
