# The HTTP/3 client

`id_effect_platform` can call `https` services over HTTP/3. Turn on the crate feature `http3`
and set one flag on the client config. The client keeps every guard of the TCP client: the
SSRF policy, the timeouts, the retries and the body cap.

```toml
id_effect_platform = { version = "0.13", features = ["http3"] }
```

```rust,ignore
use id_effect_platform::http::{HttpClientConfig, ReqwestHttpClientProvider};

let layer = ReqwestHttpClientProvider::layer_with(HttpClientConfig::default().http3(true));
```

With `http3(true)` the layer builds an `Http3Client` instead of a plain `ReqwestHttpClient`.
Code that uses the `HttpClient` capability does not change. HTTP/3 is off by default for the
client (ADR 0014 "HTTP/3"). The `service` template turns it on.

## How a request picks HTTP/3

HTTP/3 is an **upgrade**. The client never tries it blindly:

1. The first request to an `https` origin goes over TCP.
2. If the response carries `Alt-Svc: h3=":443"; ma=3600`, the client stores that the origin
   also serves HTTP/3 on UDP port 443, for `ma` seconds (24 hours when `ma` is absent).
3. The next request to that origin goes over HTTP/3. The connection is pooled and reused.

`Alt-Svc: clear` removes the entry. The cache holds 256 origins by default and evicts the least
recently used one first. The client uses only alternatives on the **same host**. An
`Alt-Svc` that names another host is ignored, so a response cannot send the client's UDP traffic
to a different machine. Only `https` origins are upgraded, never `http`.

| Setting | Default | Field |
|---|---|---|
| HTTP/3 on | off | `HttpClientConfig::http3(bool)` |
| Fallback timer | 300 ms | `config.http3.fallback_timeout` |
| Negative cache per origin | 5 minutes | `config.http3.negative_ttl` |
| `Alt-Svc` entries (LRU) | 256 | `config.http3.max_alt_svc_entries` |
| Response header section | 64 KiB | fixed |

## When UDP is blocked

Some networks drop UDP. HTTP/3 must never make a service unreachable, so the client falls back
to TCP:

- The address lookup and the QUIC handshake get **300 ms**, or the connect timeout when that is
  shorter. If they do not finish in time, the request goes over TCP at once. The TCP request is
  never delayed by more than this timer.
- The origin then goes to a **negative cache for 5 minutes**. During that time its requests go
  straight to TCP, with no 300 ms wait.
- A failure after the request was sent also falls back for `GET`, `PUT` and `DELETE`. A `POST`
  or `PATCH` that may have reached the server fails instead, because sending it again could
  repeat its effect.
- An error that shows the network changed (network unreachable or down, local address gone)
  clears the `Alt-Svc` cache, the negative cache and the pooled connections.

The TCP fallback gets what is left of the request's total timeout, so the fallback does not
extend the deadline.

## The same guards on both paths

| Guard | On HTTP/3 |
|---|---|
| SSRF policy (`SsrfPolicy`) | The URL is checked, then **every** address the host resolves to, before a UDP packet is sent. The connection uses the checked address, so DNS rebinding cannot swap it. A refusal fails the request with `HttpError::InvalidRequest`. It is not retried over TCP. |
| Total timeout | `HttpClientConfig::timeout`, or the request's own `timeout`, covers the whole exchange, body included. |
| Body cap | `HttpRequest::max_body_bytes` (default 10 MiB). A `Content-Length` over the cap fails before any body byte is read. Without one, the running total is capped. `HttpError::BodyTooLarge`. |
| Retries | The layer's `RetryingHttpClient` wraps the HTTP/3 client like the TCP one. |
| TLS | TLS 1.3 only, 0-RTT off, ALPN `h3`. The server certificate is verified against the platform CA bundle and `HttpClientConfig::root_certificates`, and the server name is checked against the origin's host. |

To trust a private CA on both paths, add its certificate:

```rust,ignore
let config = HttpClientConfig::default()
  .add_root_certificate_der(ca_der)
  .http3(true);
```

The platform CA bundle is the system bundle file: `SSL_CERT_FILE`, `NIX_SSL_CERT_FILE`, then
the usual locations on Linux, the BSDs and macOS. If no root is found, HTTP/3 is not used and
every request goes over TCP.

## Calling HTTP/3 directly

`Http3Client::execute_h3(req, port)` sends one request over HTTP/3 to a UDP port, without the
`Alt-Svc` cache and without a fallback. Use it when you know the server speaks HTTP/3, or in
tests. `Http3Client::alt_svc()` exposes the cache, so you can inspect it or seed an origin.

```rust,ignore
let client = Http3Client::from_config(&HttpClientConfig::default().http3(true));
let resp = client.execute_h3(HttpRequest::get("https://api.example.com/health"), 443);
```

## What it is built on

The client uses `h3` + `h3-quinn` (exact pins `=0.0.8` / `=0.0.10`) over its own quinn client
endpoint, with the quinn and rustls of the platform `quic` module. It never uses reqwest's
HTTP/3, because that needs an unstable cfg flag that would leak into every user's build. A test
fails the build if that flag appears in a manifest, source, workflow or Nix file.

Two differences from the TCP path:

- Response bodies are not decompressed.
- Connection-specific headers (`connection`, `keep-alive`, `proxy-connection`,
  `transfer-encoding`, `upgrade`, `te`, `host`) are not sent.

The tests in `crates/id_effect_platform/tests/http3_client.rs` run both origins in-process:
TLS over TCP, and HTTP/3 on the platform `QuicEndpoint`. They cover the upgrade, a UDP black
hole, the negative cache, the SSRF policy, the body cap, a wrong server name and the timeout.
