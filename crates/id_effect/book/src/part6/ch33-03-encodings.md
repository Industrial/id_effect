# Encodings: JSON and postcard

Every procedure speaks JSON. A procedure whose types also implement serde's `Serialize` and
`Deserialize` can speak **postcard** too, a compact binary encoding for service-to-service calls,
the process mesh and binary WebSocket frames. The codec is negotiated per request and per
connection, and an encoding the server does not know is refused: there is no silent fallback to
JSON (ADR 0014 D13).

| Codec | HTTP media type | WebSocket subprotocol | Stream framing |
|---|---|---|---|
| JSON (default) | `application/json` | `idrpc.json.v1` | NDJSON, SSE `data:` lines |
| postcard (feature `postcard`) | `application/vnd.id-effect.rpc+postcard` | `idrpc.postcard.v1` | u32 big-endian length-prefixed records |

| Need | API (`id_effect_rpc::codec`) |
|---|---|
| The request codec | `negotiate_content_type(content_type)`: absent is JSON, unknown is `Unsupported` (415) |
| The response codec | `negotiate_accept(accept, request_codec)`: absent or `*/*` is the request codec |
| The WebSocket codec | `negotiate_subprotocol(offered)`: `None` refuses the upgrade |
| A type over postcard | `WireSchema::of::<T>().with_serde()` (or any `WireSchema` + `.with_serde()`) |
| What a transport uses | `RpcMethodEntry::codecs()` → `ProcedureCodecs::{decode_input, encode_output, encode_exit}` |
| postcard frames, envelopes, records | `codec::postcard::{encode_frame, decode_frame, encode_call, decode_call, encode_result, decode_result, encode_record, split_record}` |

## Why postcard needs the type

JSON describes itself: a server can read `{"name": "Ada"}` without knowing what it is, which is
why the erased handlers every transport drives carry JSON values. postcard does not: `"Ada"` is
the byte `3` followed by `Ada`, and only the type says that a string comes next. So a procedure
takes postcard only when its schemas carry the type through serde. `with_serde()` adds that
bridge to a `WireSchema`; the registry keeps the bridges of the input and output on the
procedure's entry.

```rust
use id_effect::{DynEnv, Never, schema, succeed};
use id_effect_rpc::{Procedure, RpcGroup, WireSchema};

let short_name = || WireSchema::new(schema::max_length(schema::string::<()>(), 16)).with_serde();
let mut group = RpcGroup::new();
group.query(
  Procedure::query("Users.greet").public(),
  short_name(),
  WireSchema::new(schema::string::<()>()).with_serde(),
  |name: String, _: &mut DynEnv| succeed::<_, Never, _>(format!("Hello, {name}!")),
);
let entry = group.get("Users.greet").unwrap();
assert!(entry.codecs().is_typed());
```

A procedure registered without `with_serde()` still serves JSON; asked for postcard it answers
`415` (`CodecError::Unsupported`), naming the fix.

## Validation still runs

The bridge decodes a `T` with serde, then encodes it with the type's Schema, so the handler
receives exactly the value a JSON client would have sent, and the Schema's decode runs on it as
always. serde does not know about refinements and brands; the Schema does. A name longer than 16
characters sent as postcard is the same typed `InvalidInput` (422) failure as it is over JSON.

On the way out the bridge decodes the handler's value with the Schema and writes the `T` with
postcard. Item frames of streams go through the output bridge too; the `null` exit of a stream
kind is an empty success. A failure's error union (`{"_tag": "NotFound", …}`) travels as JSON
text inside the postcard exit: error unions are tagged unions of the contract, not one Rust type.

## Frames on the wire

The postcard frame is the frame of design note §5 in a fixed field order: `id`, `kind`, `flags`,
then `tag`, `v`, `cursor`, `credits` and `payload`, each optional. `kind` is one byte, the index
of the frame kind in this order, which never changes: `call` 0, `item` 1, `end` 2, `exit` 3,
`cancel` 4, `credit` 5, `hello` 6, `resume` 7, `ping` 8, `pong` 9. The payload is the postcard of
the procedure's type, nested as a byte string, so a transport can decode a frame before it
knows which procedure it is for:

```rust
use id_effect_rpc::codec::Codec;
use id_effect_rpc::codec::postcard::{Postcard, decode_frame, encode_frame};
use id_effect_rpc::wire::{CallId, Frame, FrameKind};

let call = Frame::call(CallId::new(1).unwrap(), "Users.greet", Postcard::encode("Ada").unwrap());
let bytes = encode_frame(&call).unwrap();
let frame = decode_frame(&bytes).unwrap();
assert_eq!(frame.kind, FrameKind::Call);
let name: String = Postcard::decode(frame.payload.as_deref().unwrap()).unwrap();
assert_eq!(name, "Ada");
```

## Hostile input

A postcard decoder is a parser facing the network, so every decoder in `codec::postcard` is
bounded:

- input over 64 KiB (`MAX_FRAME_BYTES`, C3) is refused before decoding (`CodecError::TooLarge`:
  HTTP 413, WebSocket close 1009), and a stream record whose length prefix announces more is
  refused before its bytes are buffered;
- nesting is limited to `MAX_DEPTH` (64), so a recursive type cannot exhaust the stack;
- all sequences and maps together may hold no more elements than the input could encode, so a
  five-byte "a billion empty values" cannot spin, and a length prefix never preallocates more
  than the input holds;
- strings and byte strings are checked against the remaining input before they are copied;
  trailing bytes, an unknown frame kind, reserved flag bits and an invalid procedure tag are
  refused.

`codec::postcard::decode_frame` is the entry point of the fuzz target.
