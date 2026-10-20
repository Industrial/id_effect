//! Soak (r12-e12-5-benches, T12 "10,000 concurrent streams per node, flat memory"): one node
//! serves `--streams` concurrent WebSocket server streams, each sending a heartbeat every
//! `--interval-ms`, for `--minutes` (or `--seconds`); the node's resident set size is sampled.
//!
//! ```bash
//! cargo run --release -p id_effect_rpc --features ws --example streams_soak -- \
//!   --streams 10000 --minutes 10
//! cargo run --release -p id_effect_rpc --features ws --example streams_soak -- \
//!   --streams 10000 --seconds 60
//! ```
//!
//! The node is a child process (this binary with `--serve`): the host's listener
//! (`id_effect_axum::serve_listener`, default `ConnectionLimits`) serving an [`RpcServer`] with the secure
//! WebSocket defaults ([`RpcWsConfig::default`]: 100 calls per socket, 16 sockets per client IP,
//! 10,000 per endpoint, 100 inbound messages per second per socket, pings every 20 s), whose
//! public server stream `Soak.beats` yields `{seq, at_ms}` at `start + seq × interval`. Its RSS
//! (`VmRSS` of `/proc/<pid>/status`, Linux) is what is measured, so the client's own memory is
//! not. The client (this process) opens `--per-socket` streams (default 50) on each socket and
//! spreads the sockets over source addresses `127.0.0.2`, `127.0.0.3`, … so no address holds
//! more than the per-IP cap of 16.
//!
//! **Pass** when, at the end, every stream has received every heartbeat due (no gap, no
//! duplicate, no early exit, at most one heartbeat in flight) and the RSS at the end is at most
//! 1.05 × the RSS at the mark: half the run, at most minute 5 (minute 5 of the 10-minute soak,
//! 30 s of 60 s). The mark comes after the warm-up: each socket's resume replay buffer holds
//! every heartbeat until the client's next credit grant (half a window, 16 heartbeats), so the
//! node's RSS climbs for the first minutes, bounded, before it is flat (0.12: 1.040 × from
//! minute 1, 0.997 × from minute 5 over 10 minutes). The RSS at minute 1 and the growth since
//! then are still reported (`rss_kib.at_minute_1`, `growth_since_minute_1`), not gated. Exits 1
//! on a failure; `--out FILE` also writes the result as JSON.

use std::io::{BufRead as _, BufReader, Read as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use id_effect::{DynEnv, Never, Stream, from_async, schema, succeed};
use id_effect_rpc::client::CallEvent;
use id_effect_rpc::client::ws::WsClient;
use id_effect_rpc::transport::ws::RpcWsConfig;
use id_effect_rpc::{Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// The secure default per client IP (`id_effect_axum::realtime::ws::DEFAULT_MAX_SOCKETS_PER_IP`).
const SOCKETS_PER_IP: usize = 16;

/// The RSS mark of a run of 10 minutes or more: minute 5, after the replay buffers' warm-up.
const MARK: Duration = Duration::from_secs(300);

/// Minute 1, whose RSS is reported beside the mark (not gated).
const MINUTE_1: Duration = Duration::from_secs(60);

/// Command-line settings.
#[derive(Clone, Debug)]
struct Args {
  streams: usize,
  duration: Duration,
  interval: Duration,
  per_socket: usize,
  sample_every: Duration,
  out: Option<String>,
}

fn parse_args() -> Result<Args, String> {
  let mut args = Args {
    streams: 10_000,
    duration: Duration::from_secs(600),
    interval: Duration::from_secs(1),
    per_socket: 50,
    sample_every: Duration::from_secs(10),
    out: None,
  };
  let mut it = std::env::args().skip(1);
  while let Some(flag) = it.next() {
    let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
    let number = |v: String| v.parse::<f64>().map_err(|e| format!("{flag}: {e}"));
    match flag.as_str() {
      "--streams" => args.streams = number(value()?)? as usize,
      "--minutes" => args.duration = Duration::from_secs_f64(number(value()?)? * 60.0),
      "--seconds" => args.duration = Duration::from_secs_f64(number(value()?)?),
      "--interval-ms" => args.interval = Duration::from_secs_f64(number(value()?)? / 1e3),
      "--per-socket" => args.per_socket = number(value()?)? as usize,
      "--sample-secs" => args.sample_every = Duration::from_secs_f64(number(value()?)?),
      "--out" => args.out = Some(value()?),
      other => return Err(format!("unknown argument {other}")),
    }
  }
  if args.streams == 0 || args.per_socket == 0 || args.per_socket > 100 {
    return Err("--streams must be > 0 and --per-socket in 1..=100 (the calls cap)".into());
  }
  Ok(args)
}

fn unix_ms() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |d| d.as_millis() as i64)
}

// ---------------------------------------------------------------------------------------------
// The node (child process)
// ---------------------------------------------------------------------------------------------

/// One heartbeat: its number and when the node sent it (ms since the Unix epoch).
#[derive(Clone, Debug, id_effect::Schema, serde::Serialize, serde::Deserialize)]
struct Beat {
  seq: i64,
  at_ms: i64,
}

/// `Soak.beats(interval_ms)`: `{seq, at_ms}` at `start + seq × interval`, forever.
fn group() -> RpcGroup {
  let mut group = RpcGroup::new();
  group.server_stream(
    Procedure::server_stream("Soak.beats").public(),
    WireSchema::new(schema::i64::<()>()),
    WireSchema::<Beat>::of(),
    |interval_ms: i64, _: &mut DynEnv| {
      let interval = Duration::from_millis(interval_ms.clamp(10, 60_000) as u64);
      let start = tokio::time::Instant::now();
      let mut seq = 0_i64;
      succeed::<_, Never, _>(Stream::from_pull(move |_| {
        let k = seq;
        seq += 1;
        let at = start + interval * k as u32;
        from_async(move || async move {
          tokio::time::sleep_until(at).await;
          Ok(Some(vec![Beat {
            seq: k,
            at_ms: unix_ms(),
          }]))
        })
      }))
    },
  );
  group
}

fn serve() {
  let rt = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("tokio runtime");
  rt.block_on(async {
    let server = RpcServer::new(group()).with_websocket(RpcWsConfig::default());
    let app = server.router().with_state(DynEnv::new()).into_axum();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
      .await
      .expect("bind");
    println!("listening {}", listener.local_addr().expect("addr"));
    // The parent holds our stdin: exit when it closes (the parent ended, however it ended).
    std::thread::spawn(|| {
      let mut sink = Vec::new();
      let _ = std::io::stdin().read_to_end(&mut sink);
      std::process::exit(0);
    });
    // The host's listener with its default connection limits (10,000 connections, header
    // timeout, keep-alive), as a node serves.
    id_effect_axum::serve_listener(
      listener,
      app,
      id_effect_axum::ConnectionLimits::default(),
      std::future::pending(),
    )
    .await
    .expect("serve");
  });
}

/// The node's resident set size in KiB (`VmRSS`).
fn rss_kib(pid: u32) -> Option<u64> {
  let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
  status
    .lines()
    .find_map(|l| l.strip_prefix("VmRSS:"))
    .and_then(|v| v.split_whitespace().next()?.parse().ok())
}

// ---------------------------------------------------------------------------------------------
// The client (this process)
// ---------------------------------------------------------------------------------------------

/// What one stream received.
#[derive(Default)]
struct Beats {
  /// The next `seq` expected.
  next: AtomicU64,
  /// Heartbeats out of order or repeated.
  bad: AtomicU64,
  /// The stream ended (an exit or a closed socket) before the run did.
  ended: AtomicBool,
  /// Opened at, ms since the run's epoch.
  opened_ms: AtomicU64,
}

/// Lag (receive time − send time) histogram, 1 ms buckets up to 10 s.
struct Lag {
  buckets: Vec<AtomicU64>,
}

impl Lag {
  fn new() -> Self {
    Self {
      buckets: (0..=10_000).map(|_| AtomicU64::new(0)).collect(),
    }
  }

  fn record(&self, ms: i64) {
    let i = ms.clamp(0, 10_000) as usize;
    self.buckets[i].fetch_add(1, Ordering::Relaxed);
  }

  fn quantile(&self, q: f64) -> u64 {
    let counts: Vec<u64> = self
      .buckets
      .iter()
      .map(|b| b.load(Ordering::Relaxed))
      .collect();
    let total: u64 = counts.iter().sum();
    let rank = ((total as f64) * q).ceil() as u64;
    let mut seen = 0;
    for (ms, n) in counts.iter().enumerate() {
      seen += n;
      if seen >= rank.max(1) {
        return ms as u64;
      }
    }
    10_000
  }
}

async fn open_socket(addr: SocketAddr, source: IpAddr) -> Result<WsClient, String> {
  let socket = tokio::net::TcpSocket::new_v4().map_err(|e| e.to_string())?;
  socket
    .bind(SocketAddr::new(source, 0))
    .map_err(|e| format!("bind {source}: {e}"))?;
  let tcp = socket.connect(addr).await.map_err(|e| e.to_string())?;
  let mut request = format!("ws://{addr}/rpc/ws")
    .into_client_request()
    .map_err(|e| e.to_string())?;
  request.headers_mut().insert(
    "sec-websocket-protocol",
    HeaderValue::from_static("idrpc.json.v1"),
  );
  let (ws, _) = tokio_tungstenite::client_async(request, tcp)
    .await
    .map_err(|e| e.to_string())?;
  WsClient::over(ws).await.map_err(|e| e.to_string())
}

fn spawn_node() -> (Child, SocketAddr) {
  let exe = std::env::current_exe().expect("current exe");
  let mut child = Command::new(exe)
    .arg("--serve")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()
    .expect("spawn the node");
  let mut line = String::new();
  BufReader::new(child.stdout.take().expect("stdout"))
    .read_line(&mut line)
    .expect("the node's address");
  let addr = line
    .trim()
    .strip_prefix("listening ")
    .and_then(|a| a.parse().ok())
    .unwrap_or_else(|| panic!("unexpected node output {line:?}"));
  (child, addr)
}

fn main() {
  if std::env::args().nth(1).as_deref() == Some("--serve") {
    serve();
    return;
  }
  let args = match parse_args() {
    Ok(args) => args,
    Err(e) => {
      eprintln!("streams_soak: {e}");
      std::process::exit(2);
    }
  };
  let rt = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("tokio runtime");
  let pass = rt.block_on(run(args));
  // The runtime holds the sockets' tasks; dropping it in the background keeps exit quick.
  rt.shutdown_background();
  std::process::exit(i32::from(!pass));
}

async fn run(args: Args) -> bool {
  let (mut child, addr) = spawn_node();
  let pid = child.id();
  let sockets = args.streams.div_ceil(args.per_socket);
  let epoch = Instant::now();
  let interval_ms = args.interval.as_millis() as i64;
  println!(
    "streams_soak: node pid {pid} on {addr}; {} streams on {sockets} sockets ({} per socket), \
     heartbeat every {interval_ms} ms, {:?}; rss at start {:?} KiB",
    args.streams,
    args.per_socket,
    args.duration,
    rss_kib(pid)
  );
  let lag = Arc::new(Lag::new());
  let mut beats: Vec<Arc<Beats>> = Vec::with_capacity(args.streams);
  let mut clients = Vec::with_capacity(sockets);
  let mut errors: Vec<String> = Vec::new();
  for s in 0..sockets {
    let source = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2 + (s / SOCKETS_PER_IP) as u8));
    let client = match open_socket(addr, source).await {
      Ok(c) => c,
      Err(e) => {
        errors.push(format!("socket {s}: {e}"));
        break;
      }
    };
    let n = args.per_socket.min(args.streams - s * args.per_socket);
    for _ in 0..n {
      let stat = Arc::new(Beats::default());
      stat
        .opened_ms
        .store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
      let mut call = match client.open("Soak.beats", json!(interval_ms), None).await {
        Ok(call) => call,
        Err(e) => {
          errors.push(format!("open on socket {s}: {e}"));
          stat.ended.store(true, Ordering::Relaxed);
          beats.push(stat);
          continue;
        }
      };
      let (st, lag) = (Arc::clone(&stat), Arc::clone(&lag));
      tokio::spawn(async move {
        while let Some(event) = call.next().await {
          match event {
            CallEvent::Item { value, .. } => {
              let seq = value["seq"].as_u64().unwrap_or(u64::MAX);
              if st
                .next
                .compare_exchange(seq, seq + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
              {
                st.bad.fetch_add(1, Ordering::Relaxed);
                st.next.store(seq.saturating_add(1), Ordering::Relaxed);
              }
              lag.record(unix_ms() - value["at_ms"].as_i64().unwrap_or(0));
            }
            CallEvent::Exit(_) => break,
          }
        }
        st.ended.store(true, Ordering::Relaxed);
      });
      beats.push(stat);
    }
    clients.push(client);
  }
  let opened = epoch.elapsed();
  println!(
    "streams_soak: {} streams open after {:.1} s; rss {:?} KiB",
    beats.len(),
    opened.as_secs_f64(),
    rss_kib(pid)
  );

  // Sample the node's RSS from the moment every stream is open. The mark is minute 5 (half of
  // the 10-minute soak), or half a shorter run; minute 1 is reported beside it.
  let mark = (args.duration / 2).min(MARK);
  let start = Instant::now();
  let mut samples: Vec<(f64, u64)> = Vec::new();
  let mut mark_rss = None;
  let mut minute1_rss = None;
  loop {
    let t = start.elapsed();
    let rss = rss_kib(pid).unwrap_or(0);
    samples.push((t.as_secs_f64(), rss));
    if mark_rss.is_none() && t >= mark {
      mark_rss = Some(rss);
    }
    if minute1_rss.is_none() && t >= MINUTE_1 && args.duration > MINUTE_1 {
      minute1_rss = Some(rss);
    }
    let ended = beats
      .iter()
      .filter(|b| b.ended.load(Ordering::Relaxed))
      .count();
    println!(
      "streams_soak: t={:>6.1} s rss {rss} KiB, lag p50 {} ms p99 {} ms, streams ended {ended}",
      t.as_secs_f64(),
      lag.quantile(0.50),
      lag.quantile(0.99)
    );
    if t >= args.duration {
      break;
    }
    // The next sample on the grid, and the mark itself when it falls between two.
    let mut next = (args.sample_every * samples.len() as u32).min(args.duration);
    if mark_rss.is_none() && mark < next {
      next = mark;
    }
    if minute1_rss.is_none() && args.duration > MINUTE_1 && MINUTE_1 < next {
      next = MINUTE_1;
    }
    tokio::time::sleep_until(tokio::time::Instant::from_std(start + next)).await;
  }

  // Every heartbeat due by now arrived: at least floor(elapsed / interval) of the
  // floor(elapsed / interval) + 1 sent, so at most one in flight.
  let now_ms = epoch.elapsed().as_millis() as u64;
  let interval = args.interval.as_millis().max(1) as u64;
  let mut missing = 0u64;
  let mut short_streams = 0usize;
  let (mut bad, mut ended) = (0u64, 0usize);
  let mut received = 0u64;
  for b in &beats {
    let due = (now_ms.saturating_sub(b.opened_ms.load(Ordering::Relaxed))) / interval;
    let got = b.next.load(Ordering::Relaxed);
    received += got;
    if got < due {
      missing += due - got;
      short_streams += 1;
    }
    bad += b.bad.load(Ordering::Relaxed);
    ended += usize::from(b.ended.load(Ordering::Relaxed));
  }
  let end_rss = samples.last().map_or(0, |s| s.1);
  let mark_rss = mark_rss.unwrap_or(end_rss);
  let growth = end_rss as f64 / mark_rss.max(1) as f64;
  // Reported only: the growth since minute 1 includes the replay buffers' warm-up.
  let growth_minute1 = minute1_rss.map(|m1| end_rss as f64 / m1.max(1) as f64);
  let heartbeats_ok =
    errors.is_empty() && beats.len() == args.streams && missing == 0 && bad == 0 && ended == 0;
  let rss_ok = end_rss > 0 && growth <= 1.05;
  let pass = heartbeats_ok && rss_ok;
  let peak = samples.iter().map(|s| s.1).max().unwrap_or(0);
  let result = json!({
    "streams": args.streams,
    "streams_open": beats.len(),
    "sockets": clients.len(),
    "per_socket": args.per_socket,
    "interval_ms": interval_ms,
    "duration_s": args.duration.as_secs_f64(),
    "open_s": opened.as_secs_f64(),
    "heartbeats_received": received,
    "heartbeats_missing": missing,
    "streams_short": short_streams,
    "heartbeats_out_of_order": bad,
    "streams_ended_early": ended,
    "lag_ms": {"p50": lag.quantile(0.50), "p99": lag.quantile(0.99), "p999": lag.quantile(0.999)},
    "rss_kib": {"mark_s": mark.as_secs_f64(), "at_mark": mark_rss, "end": end_rss, "peak": peak,
      "growth": growth, "at_minute_1": minute1_rss, "growth_since_minute_1": growth_minute1,
      "samples": samples.iter().map(|(t, r)| json!([t, r])).collect::<Vec<Value>>()},
    "errors": errors,
    "pass": {"heartbeats": heartbeats_ok, "rss": rss_ok, "all": pass},
  });
  println!("streams_soak: {result}");
  println!(
    "streams_soak: {} — {} streams, {received} heartbeats, missing {missing}, out of order {bad}, \
     ended early {ended}; rss {mark_rss} KiB at {:.0} s -> {end_rss} KiB at the end ({growth:.3}x, \
     limit 1.05)",
    if pass { "PASS" } else { "FAIL" },
    beats.len(),
    mark.as_secs_f64()
  );
  if let (Some(m1), Some(g1)) = (minute1_rss, growth_minute1) {
    println!(
      "streams_soak: since minute 1 (reported, not gated): {m1} KiB -> {end_rss} KiB ({g1:.3}x)"
    );
  }
  let pretty = serde_json::to_string_pretty(&result).unwrap_or_default();
  if let Some(out) = &args.out
    && let Err(e) = std::fs::write(out, pretty)
  {
    eprintln!("streams_soak: cannot write {out}: {e}");
  }
  drop(clients);
  let _ = child.kill();
  let _ = child.wait();
  pass
}
