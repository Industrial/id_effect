---
title: 'E12.1 — Postcard encoding negotiated next to JSON: r12-e12-1-postcard'
slug: r12-e12-1-postcard
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'D13: Codec negotiation per request (Content-Type / Accept: application/json default, application/vnd.id-effect.rpc+postcard) and per connection (WebSocket subprotocol names idrpc.json.v1 / idrpc.postcard.v1 reserved for r12-e12-2-rpc-ws-sse); an unknown codec answers 415 Problem.'
  - 'The postcard path decodes with serde and then runs the Schema validators of the input type, so refinements and brands still apply; encoded size is capped before decode.'
  - 'Tests `postcard_round_trip_every_kind`, `postcard_payload_still_runs_schema_refinements`, `unknown_codec_is_415`, `json_is_default`, `postcard_oversized_frame_rejected_before_decode`; `codec::postcard::decode_frame` is public for the fuzz target; book part6/ch33-03-encodings.md.'
  - 'Gate: `cargo nextest run -p id_effect_rpc --features postcard`; `cargo nextest run -p id_effect_rpc --all-features`'
  - 'Owns: `crates/id_effect_rpc/src/codec/{mod.rs,postcard.rs}`, `crates/id_effect_rpc/tests/codec.rs`, `crates/id_effect/book/src/part6/ch33-03-encodings.md`'
---

# r12-e12-1-postcard

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.1 — RPC". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r12-e12-1-kinds. r12-e12-2-mesh-rpc uses postcard on the mesh. First in the
scope-cut order after WebTransport (JSON covers every client).
