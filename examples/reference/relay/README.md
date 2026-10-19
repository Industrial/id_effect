# Relay — id_effect reference app

A 3-node chat with presence: clients talk to the nodes over two-way RPC (`id_effect_rpc`),
rooms are `id_effect_rpc::channels` topics, and the nodes share presence over the mesh
(`id_effect_node`) on the same QUIC endpoint that serves HTTP/3. Every battery comes through
`id_effect_kit`.

Status: skeleton (release 0.12.0, leaf `r12-t0-skeletons`). The application lands in leaf
`r12-w7-relay`; the CI job `reference-relay` runs it over HTTP/3, over WebSocket, and with UDP
blocked (TCP fallback).

```bash
cargo run -p relay
```

The program is written against the kit prelude:

```rust
use id_effect_kit::prelude::*;

fn banner() -> Effect<String, String> {
  succeed("relay".to_string())
}

assert!(matches!(banner().block_on(), Exit::Success(s) if s == "relay"));
```
