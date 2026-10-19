# WebTransport: RPC streams over HTTP/3

WebTransport gives a browser what a WebSocket gives it, multiplexed streams to the server, but
over HTTP/3. Each call gets its own QUIC stream, so a lost packet holds up only the call it
belongs to, and a session survives the phone moving from Wi-Fi to cellular. The RPC transport
here carries every call kind over WebTransport, with the same frames, credits, guards and resume
as the WebSocket transport. Browsers that cannot use WebTransport fall back to WebSocket.

## What you'll learn

- Serve RPC over WebTransport next to the WebSocket transport.
- What travels on the streams: records, the control stream, one stream per call.
- Which defaults protect the session: the Origin allowlist, session caps, the heartbeat.
- How a session that was lost resumes its streams.
- How the browser test drives Chromium.

## Prerequisites

- [Serving HTTP/3 from the app](./ch29-05-http3.md): the QUIC endpoint and `serve_h3`.
- [WebSocket and server-sent events](./ch33-05-websocket-sse.md) and
  [Two-way streams and resume](./ch33-07-two-way-streams.md): the frames and the resume protocol
  that WebTransport reuses.

## Serving it

Build `id_effect_rpc` with the `webtransport` feature. It turns on `http3`, `ws` and `postcard`.
Then add the route and serve HTTP/3 with WebTransport on:

```rust,ignore
use id_effect_axum::server::h3::{Http3Options, serve_h3};
use id_effect_axum::server::webtransport::{self as wt, WebTransportConfig};
use id_effect_rpc::transport::webtransport::{self, RpcWebTransportConfig};
use id_effect_rpc::transport::ws::RpcWsConfig;

let origins = ["https://app.example.com"];
let rpc = RpcWsConfig::default(); // calls, credits, the resume store
let server = RpcServer::new(group)
  .with_websocket(rpc.clone().socket(WsConfig::default().allowed_origins(origins)?));
let session = WebTransportConfig::default().allowed_origins(origins)?;
let route = webtransport::endpoint(
  server.clone(),
  RpcWebTransportConfig::default().session(session).rpc(rpc),
);
let app = server.router().route(route).with_state(env).into_axum();

let limits = ConnectionLimits::default();
let endpoint = QuicEndpoint::bind(
  addr,
  QuicEndpointConfig::new(identity).with_limits(wt::quic_limits(&limits)),
)?;
serve_h3(&endpoint, app, limits, Http3Options::default().webtransport(), shutdown).await?;
```

Three things are new:

- **The route.** `CONNECT /rpc/wt` is a typed route, public like `GET /rpc/ws`: each call runs
  its procedure's own guards against the CONNECT's head (its `Authorization`, cookies,
  principal). The access report lists it.
- **`Http3Options::webtransport`.** The HTTP/3 connections advertise extended CONNECT,
  HTTP/3 datagrams and one WebTransport session each. A CONNECT that the route accepts takes the
  connection over; plain HTTP/3 requests on that connection still reach the router. The session
  rides the endpoint's one `h3` route: nothing else is registered.
- **`wt::quic_limits`.** HTTP/3 datagrams need QUIC datagrams, which the endpoint has off by
  default. `quic_limits` turns them on (64 KiB buffered per connection) and keeps every other
  limit of `h3::quic_limits`. The setting is per endpoint, because QUIC sends its transport
  parameters before the ALPN chooses a route.

The host entry points (`ServerHandle`, `serve_router`) keep WebTransport off; use `serve_h3` on
an endpoint of your own when you need it.

Pass the same `RpcWsConfig` (or a clone) to both transports: clones share one resume store, so
a client can lose a WebTransport session and resume its streams over WebSocket, or the other way
round.

## What travels on the streams

The frames are the WebSocket transport's (`call`, `item`, `credit`, `exit`, `hello`, `resume`,
`ping`, `pong`). Only the framing differs:

| | WebSocket | WebTransport |
|---|---|---|
| Framing | one message per frame | one record per frame: u32 big-endian length, then the codec bytes |
| Calls | multiplexed by id on one socket | one bidirectional stream per call |
| `hello`, `ping`, `pong` | on the socket | on the control stream |
| Codec | subprotocol `idrpc.json.v1` / `idrpc.postcard.v1` | `?codec=idrpc.postcard.v1` on the CONNECT, JSON otherwise |
| Heartbeat | WebSocket ping, 20 s | `ping` frames on the control stream, 20 s |

The client opens the **control stream** first: a bidirectional stream whose first record is a
connection frame (id 0), usually a `ping`. The server's `hello` comes back on it. Every other
stream belongs to the call id of its first record. A `call` frame opens the call there; its
items, credits and exit travel on that stream; the server finishes the stream after the `exit`.

The rules are strict, and a session that breaks one is closed (`H3_GENERAL_PROTOCOL_ERROR`):

- a second control stream;
- a record for another call on a call's stream;
- a length prefix over 64 KiB, refused before a byte of the record is buffered;
- a frame that does not decode, or a stream that ends inside a record.

## Defaults that protect the session

| Limit | Default |
|---|---|
| Origin allowlist | empty: every browser origin refused (**403**); requests without `Origin` pass to the guards |
| Sessions | 10,000 per endpoint, 16 per client IP (**429**) |
| Sessions per connection | 1 |
| Streams per session | the endpoint's QUIC credits: 100 at once |
| Calls per session | 100 (`RpcWsConfig::max_calls`), past it the call exits `ResourceExhausted` |
| Frame | 64 KiB |
| Credits | 32 items per call each way, as on WebSocket |
| Heartbeat | a `ping` every 20 s; two silent intervals close the session, as does no control stream |

The Origin check is the one WebSocket upgrades get (ADR 0013 R9, ADR 0014 C4), with the same
`OriginPolicy`. Cross-site WebTransport hijacking is the same attack as cross-site WebSocket
hijacking: a page on another site opening a session that carries the user's cookies.

## Resume after a lost session

Server streams and two-way streams keep running detached when the session goes, for the resume
retention (30 s by default), with their producers stopped at the credit window. The client opens
a new session and starts its control stream with `resume`:

```json
{"id": 0, "kind": "resume", "payload": {"session": "<from the last hello>", "calls": [{"id": 1, "cursor": "c1.AgAAAAAAAAAF"}]}}
```

The control stream answers with the new connection's `hello`, then the `hello` naming the
resumed session, then each call's replay after its cursor. Those replays come on the control
stream because the call has no stream in the new session. A client moves a call onto a stream of
its own by sending the call's next frame (its `credit`) on a new stream; from then on that call's
frames arrive there. The rest is the WebSocket resume: the guards run again, a buffer that no
longer holds the cursor answers `ResumeExpired`, and client items in flight are not replayed.

## The browser test

`tests/browser.rs` drives headless Chromium from Rust over the DevTools protocol, with
`headless_chrome` (its tungstenite is the version the tree already has; `chromiumoxide` would
add a second one). The page's client prefers WebTransport, then WebSocket:

1. with WebTransport served on a 13-day ECDSA P-256 certificate pinned by
   `serverCertificateHashes` (Chromium accepts self-signed certificates that way only when they
   are ECDSA and valid for at most 14 days), the two-way call runs over WebTransport;
2. with nothing listening on the WebTransport port, the same page falls back to WebSocket.

```bash
ID_EFFECT_REQUIRE_BROWSER=1 cargo nextest run -p id_effect_rpc --features webtransport --test browser
```

With the variable set, a missing Chromium or `openssl` fails the test instead of skipping it.
Without it the test does nothing. `id-effect dev` makes the same kind of certificate for an app's
development profile.

## Summary

- `webtransport::endpoint` plus `Http3Options::webtransport` on an endpoint with
  `wt::quic_limits` serves RPC over WebTransport: one QUIC stream per call.
- Frames are the WebSocket transport's, as length-prefixed records; the control stream carries
  the connection frames.
- Origins are refused unless allowed, sessions are capped, and a heartbeat closes silent ones.
- A lost session resumes its streams over WebTransport or WebSocket from one shared store.
