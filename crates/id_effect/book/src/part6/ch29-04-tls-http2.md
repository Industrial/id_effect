# Serving HTTPS and HTTP/2 from the app

Most deployments put a load balancer in front of the service and let it terminate TLS. Some
cannot: a single VM with no proxy, an internal service that must be encrypted end to end, or an
app that wants to offer HTTP/3, which browsers only discover through an `Alt-Svc` header sent over
HTTPS. For those, the host serves TLS itself, on the same port and with the same router, and
negotiates HTTP/2 with clients that speak it.

## What you'll learn

- Turn on TLS for the host from two environment variables, or from a certificate you hold.
- Rotate a certificate without a restart, and what happens to connections already open.
- Which limits protect a TLS listener, and how HTTP/2 multiplexing is capped.
- When to leave TLS to the load balancer instead.

## Prerequisites

- [Axum host (`id_effect_axum`)](../part2/ch07-08-axum-host.md): `HostConfig`, `ServerHandle`
  and the connection limits.
- [Middleware, static files and HTTP caching](./ch29-03-middleware.md).

## When you need this

| Topology | TLS in the app? |
|---|---|
| Load balancer or ingress terminates TLS, forwards plain HTTP | No. Keep the default (plain HTTP) and configure trusted proxies. |
| The app is reached directly (VM, bare metal, an internal hop that must be encrypted) | Yes. |
| The app should offer HTTP/3 to browsers directly | Yes: `Alt-Svc` is only honoured on HTTPS. |

Without TLS configured, nothing changes: the host serves HTTP/1.1 on plain TCP, as before.

## Turning it on

Build with the `tls` feature:

```toml
[dependencies]
id_effect_axum = { version = "0.13", features = ["tls"] }
```

Then point the host at a PEM certificate chain (leaf first) and its private key:

```bash
TLS_CERT_FILE=/etc/app/tls/chain.pem TLS_KEY_FILE=/etc/app/tls/key.pem cargo run --release
```

`HostConfig::config()` reads both (set both or neither; one alone is a config error), and every
host entry point (`serve`, `serve_until_shutdown`, `ServerHandle::start`, `serve_router`) now
answers TLS on the bound port. The listener offers TLS 1.3 and TLS 1.2 and, through ALPN, `h2`
then `http/1.1`: a browser gets HTTP/2, `curl --http1.1` gets HTTP/1.1, and a client that sends
no ALPN at all gets HTTP/1.1. Plain HTTP sent to the TLS port is not answered. Early data (0-RTT)
is off: it can be replayed.

If `TLS_CERT_FILE` is set but the binary was built without the `tls` feature, the host refuses to
start instead of silently serving plain HTTP.

## Rotating certificates

The files are re-read every 60 seconds (`TLS_RELOAD_SECS`; `0` never re-reads them). A change is
detected by content, so a renewal tool that rewrites the files in place is enough. When the new
pair parses, the **next** handshake presents it; connections already open keep the certificate
they were established with until they close. A half-written or mismatched pair is logged and the
current certificate stays in use until the next check.

In code, the same thing is `HostTls::files` (or `HostTls::Files { reload_every, .. }`):

```rust,no_run
// no_run: the certificate files do not exist here.
use axum::Router;
use id_effect_axum::server::HostTls;
use id_effect_axum::{HostConfig, ServerHandle};

# async fn run() -> Result<(), id_effect_axum::HostError> {
let config = HostConfig::default().with_tls(HostTls::files(
  "/etc/app/tls/chain.pem",
  "/etc/app/tls/key.pem",
));
let server = ServerHandle::start(&config, Router::new()).await?;
# server.shutdown().await;
# Ok(())
# }
```

When you already hold the certificate (generated at startup, fetched from a secret store, or an
`id_effect_platform::tls::CertReloader` shared with the QUIC endpoint), hand over its `Identity`.
Every handshake reads the identity's current certificate, so swapping it through the reloader
reaches new connections here too:

```rust
use id_effect_axum::HostConfig;
use id_effect_axum::server::HostTls;
use id_effect_axum::server::tls::ServerTls;
use id_effect_platform::tls::{Identity, NodeCertificate};

let identity = Identity::from_certificate(&NodeCertificate::self_signed("localhost").unwrap())
  .unwrap();
let config = HostConfig::default()
  .with_tls(HostTls::provided(ServerTls::from_identity(&identity).unwrap()));
assert!(config.tls.is_some());
```

`ServerTls::builder(resolver)` takes any rustls certificate resolver for the cases an identity
does not cover, with `.tls13_only()` to refuse TLS 1.2 clients, `.alpn(..)` for an extra
protocol and `.configure(..)` to adjust the finished rustls config. A connection that negotiates
anything other than `h2` or `http/1.1` is closed right after the handshake and never reaches the
router. That is exactly what ACME's TLS-ALPN-01 challenge needs (see
[Certificates that rotate themselves](./ch29-09-certificates-acme.md)):

```rust,ignore
// ignore: needs a configured `id_effect_platform::tls::acme::Acme` (feature `acme`).
let tls = ServerTls::builder(Arc::new(acme.resolver()))
  .configure(move |config| acme.serve_tls_alpn01(config)) // appends `acme-tls/1`
  .build()?;
let config = HostConfig::default().with_tls(HostTls::provided(tls));
```

A rustls config built elsewhere is accepted as is by `ServerTls::from_rustls_config` (early data
is still turned off). Without the host, serve a listener directly with
`server::serve_tls_listener(listener, app, limits, tls, shutdown)`.

## Limits on a TLS listener

The connection limits from plain HTTP apply unchanged, and two more close the gaps TLS and
HTTP/2 open:

| Limit | Default | Loosened only by |
|---|---|---|
| TLS handshake must finish within | 10 s | `dangerous_no_tls_handshake_timeout()` |
| First request (any protocol) and, on HTTP/1, each request head within | 10 s | `dangerous_no_header_read_timeout()` |
| Connections served at once (handshakes included) | 10,000 | `dangerous_unlimited_connections()` |
| Requests per connection (HTTP/1: `Connection: close`; HTTP/2: `GOAWAY`, open streams finish) | 1,000 | `dangerous_unlimited_requests_per_connection()` |
| HTTP/2 streams open at once on one connection | 100 | `dangerous_unlimited_concurrent_streams()` |

```rust
use std::time::Duration;

use id_effect_axum::ConnectionLimits;

let limits = ConnectionLimits::new()
  .tls_handshake_timeout(Duration::from_secs(5))
  .max_concurrent_streams(64);
# let _ = limits;
```

A client that opens a TCP connection and never says anything is closed after the handshake
timeout, and while it waits it holds one of the connection slots, so it cannot crowd out others
for long. HTTP/2 lets one connection carry many requests at once; past the stream cap, further
requests wait at the client until a stream finishes. Every `dangerous_*` call is recorded in the
posture registry, which the prod posture check reads.

Graceful shutdown covers TLS too: on shutdown the listener stops accepting, clients still in the
handshake are dropped, HTTP/1 connections finish their request, and HTTP/2 connections receive a
`GOAWAY` and finish the streams already open.

## HTTP/3 and the posture warning

`HostConfig::http3` (environment `HTTP3`, default `true`) asks for HTTP/3 next to TCP when the
binary has the `http3` feature. HTTP/3 needs a certificate, and the `Alt-Svc` header that tells
browsers about it is only honoured over HTTPS. A host with HTTP/3 on and no TLS therefore logs a
posture warning at startup (at `warn` in the `prod` profile) and sends no `Alt-Svc`; configure
TLS or set `HTTP3=false`.

## Testing

Tests in `crates/id_effect_axum/tests/tls_h2.rs` issue certificates from a throwaway CA and talk
to a real listener on `127.0.0.1:0`: an HTTP/2 and an HTTP/1.1 round trip, a TLS 1.2 client
refused by `tls13_only()`, a certificate rotated on disk and seen only by new connections, six
parallel requests on a connection capped at two streams, and a silent client closed by the
handshake timeout:

```bash
cargo nextest run -p id_effect_axum --features tls --test tls_h2
```

## Summary

TLS is a property of the listener, not of the router: the same routes, middleware and limits
serve plain HTTP, HTTPS with HTTP/1.1, and HTTPS with HTTP/2. The certificate comes from an
identity that every handshake reads, so rotation needs no restart and never disturbs connections
already open. The new exposure TLS and HTTP/2 bring (slow handshakes, multiplexed streams) is
bounded by default, and loosening it is a named, recorded opt-out.

## Next steps

- [Certificates that rotate themselves: reload and ACME](./ch29-09-certificates-acme.md)
- [Application host](./ch30-00-application.md)
