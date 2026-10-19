# Serving HTTP/3 from the app

HTTP/3 runs HTTP over QUIC on UDP. Each request is its own QUIC stream, so a lost packet stalls
only the request it belongs to, not every request on the connection as on TCP; the handshake is
one round trip shorter; and a phone that moves from Wi-Fi to cellular keeps its connection. The
host serves HTTP/3 next to HTTPS on TCP, from the same router, with one switch to turn it off.

## What you'll learn

- Turn HTTP/3 on (it is on whenever the host serves TLS from certificate files) and off.
- How browsers find it: `Alt-Svc` on the TCP responses.
- Which limits protect the UDP side, how the client IP is found, and why 0-RTT stays off.
- How to serve HTTP/3 on a QUIC endpoint of your own.

## Prerequisites

- [Serving HTTPS and HTTP/2 from the app](./ch29-04-tls-http2.md): `HostConfig::tls`, the
  certificate files and the connection limits.
- [The HTTP/3 client](./ch29-08-http3-client.md), for the other end.

## Turning it on

Build with the `http3` feature (it implies `tls`) and give the host a certificate:

```toml
[dependencies]
id_effect_axum = { version = "0.12", features = ["http3"] }
```

```bash
TLS_CERT_FILE=/etc/app/chain.pem TLS_KEY_FILE=/etc/app/key.pem PORT=443 ./app
```

That is all. `HostConfig::http3` is `true` by default, so every host entry point
(`ServerHandle::start`, `serve_router`, `server_layer`, `serve_until_shutdown`) now:

1. binds UDP on **the same port number** as TCP (443/udp next to 443/tcp), presenting the same
   certificate; a reload of the files reaches both listeners for new connections;
2. serves the **same router** over HTTP/3: typed routes, guards, middleware, the RPC endpoint and
   the health routes behave exactly as over TCP;
3. adds `Alt-Svc: h3=":443"; ma=86400` to every TCP response, which tells browsers (and the
   platform `Http3Client`) to try HTTP/3 on their next request;
4. drains both on shutdown: TCP connections finish their requests, HTTP/3 connections get a
   `GOAWAY` and finish theirs, and the host returns once both are done.

To turn it off, set one flag:

```bash
HTTP3=false ./app        # or HostConfig { http3: false, .. }
```

TCP is never turned off: clients that cannot use UDP (blocked by a firewall, an old proxy) keep
using HTTP/2 or HTTP/1.1 on the same port, and clients fall back by themselves.

Without TLS the host serves no HTTP/3 and sends no `Alt-Svc` (browsers ignore it over plain
HTTP); the [posture warnings](./ch29-04-tls-http2.md) say so at startup. TLS settings handed over
ready-made (`HostTls::Provided`) carry no certificate identity QUIC can share, so that host
serves TCP only and logs why; use certificate files, or the building block below.

## What clients see

```text
$ curl -sk --http2 -D - https://localhost:8443/version
HTTP/2 200
alt-svc: h3=":8443"; ma=86400
...
$ curl -sk --http3-only https://localhost:8443/version
HTTP/3.0
```

Example `092_http3` serves exactly this (`cargo run -p id_effect_axum --features http3 --example
092_http3`); CI runs it against `h3spec`, a suite of QUIC and HTTP/3 error cases, and against
`curl --http3-only` (job `http3-conformance`).

## Limits on the UDP side

The [`ConnectionLimits`](./ch29-04-tls-http2.md) of the TCP listener apply to HTTP/3 too:

| Limit | On HTTP/3 |
|---|---|
| `max_connections` (10,000) | HTTP/3 connections served at once (a pool of its own, the same size). Further connections wait in the endpoint's backlog of 64, then are refused. |
| `header_read_timeout` (10 s) | The first request of a connection must arrive in time, and so must every request's header section; a silent connection is closed, a stalled stream dropped. |
| `max_requests_per_connection` (1,000) | The n-th request starts a graceful close (`GOAWAY`); the requests already open finish. |
| `max_concurrent_streams` (100) | QUIC stream credits: requests a client may have open at once, capped at the endpoint ceiling of 100. |
| `tls_handshake_timeout` (10 s) | Bounds the QUIC handshake (capped at the endpoint's 10 s). |

Under these sit the QUIC endpoint's own limits (ADR 0014): address validation (Retry) before any
handshake work, at most 256 handshakes in progress, a 30 s idle timeout, bounded receive windows
and no datagrams. Request header sections are capped at 64 KiB. Body limits are the app's own
(`DefaultBodyLimit`, request caps), as on TCP.

## The client IP

Every HTTP/3 request carries `ConnectInfo<SocketAddr>` with the QUIC peer's address, so
`ClientIp` and per-IP rate limits work unchanged, including the trusted-proxy rules: with no
trusted proxies `X-Forwarded-For` is ignored, and behind a trusted proxy the rightmost untrusted
entry is the client.

## 0-RTT stays off

QUIC can carry a request in the very first packet of a resumed connection (0-RTT). Those bytes
can be captured and replayed by an attacker, so a `POST` sent in 0-RTT could run twice. The host
accepts no early data. `Http3Options::dangerous_allow_zero_rtt_for_queries()` is the recorded
opt-out for a future endpoint that offers it: it would admit `GET`, `HEAD` and `OPTIONS` only,
never a mutation.

## An endpoint of your own

`serve_h3` serves a router on any platform `QuicEndpoint`, for instance one shared with the
cluster mesh (ALPN `idm/1`) or one with a certificate resolver of your own:

```rust,ignore
use id_effect_axum::server::h3::{Http3Options, quic_limits, serve_h3};
use id_effect_platform::quic::{QuicEndpoint, QuicEndpointConfig};

let limits = ConnectionLimits::default();
let endpoint = QuicEndpoint::bind(addr, QuicEndpointConfig::new(identity).with_limits(quic_limits(&limits)))?;
serve_h3(&endpoint, app, limits, Http3Options::default(), shutdown).await?;
```

An endpoint routes `h3` once: a second `serve_h3` on it fails, and after `serve_h3` returns the
endpoint closes new `h3` connections.

## Performance under packet loss

The release measures HTTP/3 against HTTP/2 with 64 concurrent 16 KiB responses on one
connection, at 1 % loss and 5 ms delay each way (ADR 0014 D9). The first measurement (a
userspace loss proxy on loopback, p99 per response, median of three runs of 200 rounds) found
that the congestion controller decides the outcome far more than head-of-line blocking does:

| Stack | p99 |
|---|---|
| HTTP/2 over TCP (TCP modelled in the proxy: CUBIC-style window, SACK recovery) | 609 ms |
| HTTP/3, quinn's default controller (CUBIC; what the endpoint uses today) | 1,054 ms |
| HTTP/3 with quinn's BBR | 31 ms |

At 1 % random loss a loss-based controller keeps shrinking its window on both transports; BBR
does not treat random loss as congestion. On a clean network HTTP/2 was ahead (p99 19 ms
against 78 ms with CUBIC and 30 ms with BBR). The 0.12 benchmark report has the numbers from the
CI runner with kernel `netem`.

## When to leave HTTP/3 to the load balancer

If a load balancer terminates TLS, let it terminate HTTP/3 as well and turn the app's off
(`HTTP3=false`); put `AltSvcLayer::h3(<the balancer's port>)` on the router only if the balancer
does not add `Alt-Svc` itself.
