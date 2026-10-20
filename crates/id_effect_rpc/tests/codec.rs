//! Codec negotiation (ADR 0014 D13, design note §7): JSON is the default, an unknown codec is
//! 415, and (feature `postcard`) every kind round-trips over postcard frames through the typed
//! bridge, with the Schema's refinements still applied and oversized frames refused before
//! decoding.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::{DynEnv, Never, schema, succeed};
use id_effect_rpc::codec::{
  CodecError, CodecId, negotiate_accept, negotiate_content_type, negotiate_subprotocol,
};
use id_effect_rpc::{Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::{Value, json};
use tower::ServiceExt;

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>())
}

fn text() -> WireSchema<String> {
  WireSchema::new(schema::string::<()>())
}

fn app() -> axum::Router {
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Todo.get").public(),
    int(),
    text(),
    |id: i64, _: &mut DynEnv| succeed::<_, Never, _>(format!("todo {id}")),
  );
  RpcServer::new(group)
    .mount_default()
    .with_state(DynEnv::new())
    .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4000))))
}

async fn post(content_type: &str, body: &'static str) -> (StatusCode, String, Value) {
  let request = Request::post("/rpc/Todo.get")
    .header("content-type", content_type)
    .header("x-rpc", "1")
    .body(Body::from(body))
    .unwrap();
  let response = app().oneshot(request).await.unwrap();
  let status = response.status();
  let content_type = response
    .headers()
    .get("content-type")
    .map(|v| v.to_str().unwrap().to_owned())
    .unwrap_or_default();
  let bytes = response.into_body().collect().await.unwrap().to_bytes();
  (
    status,
    content_type,
    serde_json::from_slice(&bytes).unwrap_or(Value::Null),
  )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_is_default() {
  assert_eq!(CodecId::ALL[0], CodecId::Json);
  assert_eq!(negotiate_content_type(None), Ok(CodecId::Json));
  assert_eq!(negotiate_accept(None, CodecId::Json), Ok(CodecId::Json));
  assert_eq!(
    negotiate_accept(Some("*/*"), CodecId::Json),
    Ok(CodecId::Json)
  );
  let (status, content_type, body) = post("application/json", "7").await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(content_type, "application/json");
  assert_eq!(
    body,
    json!({"tag": "Todo.get", "exit": {"_tag": "Success", "value": "todo 7"}})
  );
  // A body-less GET is JSON too.
  let get = Request::get("/rpc/Todo.get?input=8")
    .body(Body::empty())
    .unwrap();
  let response = app().oneshot(get).await.unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  assert_eq!(response.headers()["content-type"], "application/json");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_codec_is_415() {
  for media_type in ["application/cbor", "application/msgpack", "text/plain"] {
    let (status, content_type, body) = post(media_type, "7").await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{media_type}");
    assert_eq!(content_type, "application/problem+json");
    assert!(
      body["detail"]
        .as_str()
        .unwrap()
        .contains("unsupported codec"),
      "{body}"
    );
  }
  assert_eq!(
    negotiate_content_type(Some("application/cbor")),
    Err(CodecError::Unsupported("application/cbor".into()))
  );
  assert!(matches!(
    negotiate_accept(Some("application/cbor"), CodecId::Json),
    Err(CodecError::Unsupported(_))
  ));
  // No silent fallback on WebSocket either: an unknown subprotocol refuses the upgrade.
  assert_eq!(negotiate_subprotocol(["idrpc.cbor.v1", "chat"]), None);
}

#[cfg(feature = "postcard")]
mod postcard {
  use super::*;

  use futures_util::StreamExt as _;
  use id_effect::Stream;
  use id_effect_rpc::cancel::CallControl;
  use id_effect_rpc::codec::postcard::{
    MAX_FRAME_BYTES, Postcard, decode_client_payload, decode_exit, decode_frame, decode_result,
    encode_frame, encode_record, encode_result, encode_server_frame, split_record,
  };
  use id_effect_rpc::codec::{Codec, ProcedureCodecs};
  use id_effect_rpc::handler::{CallContext, CallOutcome};
  use id_effect_rpc::kinds::{CallFrames, Duplex, Subscription, SubscriptionItem};
  use id_effect_rpc::server::dispatch;
  use id_effect_rpc::wire::{CallId, Cursor, Frame, FrameKind, RpcFailure, WireExit, WireResult};
  use proptest::prelude::*;
  use serde::Serialize;
  use serde::de::DeserializeOwned;

  const PC: CodecId = CodecId::Postcard;

  fn typed_int() -> WireSchema<i64> {
    int().with_serde()
  }

  fn typed_text() -> WireSchema<String> {
    text().with_serde()
  }

  fn person() -> WireSchema<(String, i64)> {
    WireSchema::with_fields(
      schema::struct_(
        "name",
        schema::min_length(schema::string::<()>(), 1),
        "age",
        schema::range(schema::i64::<()>(), 0, 150),
      ),
      &["name", "age"],
    )
    .with_serde()
  }

  fn group() -> RpcGroup {
    let mut group = RpcGroup::new();
    group.query(
      Procedure::query("Pc.get").public(),
      typed_int(),
      typed_text(),
      |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(format!("n{n}")),
    );
    group.query(
      Procedure::query("Pc.short").public(),
      WireSchema::new(schema::max_length(schema::string::<()>(), 3)).with_serde(),
      typed_text(),
      |s: String, _: &mut DynEnv| succeed::<_, Never, _>(s),
    );
    group.query(
      Procedure::query("Pc.person").public(),
      person(),
      person(),
      |(name, age): (String, i64), _: &mut DynEnv| {
        succeed::<_, Never, _>((name.to_uppercase(), age + 1))
      },
    );
    group.query(
      Procedure::query("Pc.untyped").public(),
      int(),
      text(),
      |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n.to_string()),
    );
    group.mutation(
      Procedure::mutation("Pc.len").public(),
      typed_text(),
      typed_int(),
      |s: String, _: &mut DynEnv| succeed::<_, Never, _>(i64::try_from(s.len()).unwrap()),
    );
    group.server_stream(
      Procedure::server_stream("Pc.count").public(),
      typed_int(),
      typed_int(),
      |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
    );
    group.client_stream(
      Procedure::client_stream("Pc.sum").public(),
      typed_int(),
      typed_int(),
      |items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| items.run_fold(0, |a, b| a + b),
    );
    group.bidi(
      Procedure::bidi("Pc.shout").public(),
      typed_text(),
      typed_text(),
      |_: &mut DynEnv| {
        succeed::<_, Never, _>(Duplex::new(|items: Stream<String, Never, DynEnv>| {
          items.map(|s| s.to_uppercase())
        }))
      },
    );
    group.subscription(
      Procedure::subscription("Pc.feed").public(),
      typed_text(),
      typed_text(),
      |prefix: String, _after: Option<Cursor>, _: &mut DynEnv| {
        let items = (1..=2_u64)
          .map(|n| SubscriptionItem::new(format!("{prefix}{n}")).with_cursor(Cursor::events(n)))
          .collect();
        succeed::<_, Never, _>(Subscription::new(Stream::from_vec(items)))
      },
    );
    group
  }

  fn id(n: u64) -> CallId {
    CallId::new(n).unwrap()
  }

  fn ctx() -> CallContext {
    CallContext::new(DynEnv::new(), "cid-postcard")
  }

  /// The client's bytes of a `call` frame.
  fn call_frame<T: Serialize>(call: u64, tag: &str, input: Option<&T>) -> Vec<u8> {
    let mut frame = Frame::new(id(call), FrameKind::Call);
    frame.tag = Some(tag.to_owned());
    frame.payload = input.map(|i| Postcard::encode(i).unwrap());
    encode_frame(&frame).unwrap()
  }

  /// The client's bytes of an `item` frame.
  fn item_frame<T: Serialize>(call: u64, item: &T) -> Vec<u8> {
    encode_frame(
      &Frame::new(id(call), FrameKind::Item).with_payload(Postcard::encode(item).unwrap()),
    )
    .unwrap()
  }

  /// Server side: decode a client frame, resolve its procedure (for `call`), and decode its
  /// payload through the procedure's bridge.
  fn receive<'g>(
    group: &'g RpcGroup,
    tag: &str,
    bytes: &[u8],
  ) -> (&'g id_effect_rpc::RpcMethodEntry, Frame<Vec<u8>>, Value) {
    let frame = decode_frame(bytes).unwrap();
    let entry = dispatch::resolve(group, frame.tag.as_deref().unwrap_or(tag), frame.v).unwrap();
    let value = decode_client_payload(entry.codecs(), frame.payload.as_deref()).unwrap();
    (entry, frame, value)
  }

  /// Server side: a unary outcome as the postcard `WireResult` the client reads.
  fn reply(codecs: &ProcedureCodecs, call: u64, tag: &str, outcome: &CallOutcome) -> Vec<u8> {
    let exit = codecs.encode_exit(PC, &outcome.exit).unwrap();
    encode_result(&WireResult::new(Some(id(call)), tag, exit)).unwrap()
  }

  fn success<T: DeserializeOwned>(exit: &WireExit<Vec<u8>, Value>) -> T {
    match exit {
      WireExit::Success { value } => Postcard::decode(value).unwrap(),
      WireExit::Failure { error } => panic!("failed: {error}"),
    }
  }

  /// What a client reads from a stream call: its items (with cursors) and its exit.
  type StreamRead<T> = (Vec<(Option<Cursor>, T)>, WireExit<Vec<u8>, Value>);

  /// Client side: the typed items and the exit of a stream call's postcard frames.
  fn read_stream<T: DeserializeOwned>(frames: &[Vec<u8>]) -> StreamRead<T> {
    let mut items = Vec::new();
    for bytes in &frames[..frames.len() - 1] {
      let frame = decode_frame(bytes).unwrap();
      assert_eq!(frame.kind, FrameKind::Item);
      let value = Postcard::decode(frame.payload.as_deref().unwrap()).unwrap();
      items.push((frame.cursor, value));
    }
    let exit = decode_frame(frames.last().unwrap()).unwrap();
    assert_eq!(exit.kind, FrameKind::Exit);
    (
      items,
      decode_exit(exit.payload.as_deref().unwrap()).unwrap(),
    )
  }

  async fn encode_frames(codecs: &ProcedureCodecs, frames: CallFrames) -> Vec<Vec<u8>> {
    frames
      .map(|frame| encode_server_frame(codecs, &frame).unwrap())
      .collect()
      .await
  }

  async fn unary<I: Serialize, O: DeserializeOwned>(group: &RpcGroup, tag: &str, input: &I) -> O {
    let (entry, frame, value) = receive(group, tag, &call_frame(5, tag, Some(input)));
    assert_eq!(frame.id, id(5));
    let outcome = dispatch::start_unary(entry, value, ctx(), CallControl::none())
      .unwrap()
      .await;
    let result = decode_result(&reply(entry.codecs(), 5, tag, &outcome)).unwrap();
    assert_eq!((result.id, result.tag.as_str()), (Some(id(5)), tag));
    success(&result.exit)
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn postcard_round_trip_every_kind() {
    let group = group();

    // query and mutation
    let got: String = unary(&group, "Pc.get", &7_i64).await;
    assert_eq!(got, "n7");
    let got: i64 = unary(&group, "Pc.len", &"four".to_owned()).await;
    assert_eq!(got, 4);
    let got: (String, i64) = unary(&group, "Pc.person", &("ada".to_owned(), 36_i64)).await;
    assert_eq!(got, ("ADA".to_owned(), 37));

    // server stream
    let (entry, _, value) = receive(&group, "", &call_frame(6, "Pc.count", Some(&3_i64)));
    let frames =
      dispatch::start_server_stream(entry, id(6), value, ctx(), CallControl::none(), 4).unwrap();
    let frames = encode_frames(entry.codecs(), frames).await;
    let (items, exit) = read_stream::<i64>(&frames);
    assert_eq!(items, [(None, 1), (None, 2), (None, 3)]);
    assert_eq!(exit, WireExit::Success { value: Vec::new() });

    // client stream: a `call` without payload, then items
    let (entry, _, value) = receive(&group, "", &call_frame::<()>(7, "Pc.sum", None));
    assert_eq!(value, Value::Null);
    let (tx, run) = dispatch::start_client_stream(entry, ctx(), CallControl::none(), 8).unwrap();
    for n in [1_i64, 2, 39] {
      let (_, frame, value) = receive(&group, "Pc.sum", &item_frame(7, &n));
      assert_eq!(frame.kind, FrameKind::Item);
      assert!(tx.send(value).await);
    }
    drop(tx);
    let outcome = run.await;
    let result = decode_result(&reply(entry.codecs(), 7, "Pc.sum", &outcome)).unwrap();
    assert_eq!(success::<i64>(&result.exit), 42);

    // bidi
    let (entry, _, _) = receive(&group, "", &call_frame::<()>(8, "Pc.shout", None));
    let (tx, frames) = dispatch::start_bidi(entry, id(8), ctx(), CallControl::none(), 8).unwrap();
    for word in ["hi", "there"] {
      let (_, _, value) = receive(&group, "Pc.shout", &item_frame(8, &word.to_owned()));
      assert!(tx.send(value).await);
    }
    drop(tx);
    let frames = encode_frames(entry.codecs(), frames).await;
    let (items, exit) = read_stream::<String>(&frames);
    assert_eq!(items, [(None, "HI".to_owned()), (None, "THERE".to_owned())]);
    assert!(exit.is_success());

    // subscription: cursors survive the codec
    let (entry, _, value) = receive(&group, "", &call_frame(9, "Pc.feed", Some(&"e".to_owned())));
    let frames =
      dispatch::start_subscription(entry, id(9), value, None, ctx(), CallControl::none(), 4)
        .unwrap();
    let frames = encode_frames(entry.codecs(), frames).await;
    let (items, exit) = read_stream::<String>(&frames);
    assert_eq!(
      items,
      [
        (Some(Cursor::events(1)), "e1".to_owned()),
        (Some(Cursor::events(2)), "e2".to_owned())
      ]
    );
    assert!(exit.is_success());
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn postcard_payload_still_runs_schema_refinements() {
    let group = group();
    let tag = "Pc.short";
    // serde accepts any string; the Schema's `max_length(3)` refuses it.
    let (entry, _, value) = receive(
      &group,
      tag,
      &call_frame(1, tag, Some(&"toolong".to_owned())),
    );
    let outcome = dispatch::start_unary(entry, value, ctx(), CallControl::none())
      .unwrap()
      .await;
    assert_eq!(outcome.status, StatusCode::UNPROCESSABLE_ENTITY);
    let Some(RpcFailure::InvalidInput { issues }) = outcome.reserved() else {
      panic!("expected InvalidInput, got {:?}", outcome.exit);
    };
    assert_eq!(issues.len(), 1);
    // The failure crosses postcard as the reserved tagged union.
    let result = decode_result(&reply(entry.codecs(), 1, tag, &outcome)).unwrap();
    let WireExit::Failure { error } = result.exit else {
      panic!("expected a failure");
    };
    assert_eq!(error["_tag"], "InvalidInput");

    // A struct refinement: age outside 0..=150, empty name.
    let tag = "Pc.person";
    let bad = ("".to_owned(), 200_i64);
    let (entry, _, value) = receive(&group, tag, &call_frame(2, tag, Some(&bad)));
    let outcome = dispatch::start_unary(entry, value, ctx(), CallControl::none())
      .unwrap()
      .await;
    let Some(RpcFailure::InvalidInput { issues }) = outcome.reserved() else {
      panic!("expected InvalidInput, got {:?}", outcome.exit);
    };
    let paths: Vec<_> = issues.iter().map(|i| i.path.as_str()).collect();
    assert!(
      paths.contains(&"/name") && paths.contains(&"/age"),
      "{paths:?}"
    );

    // Bytes that are not the input type at all are a decode error (the transport's 400).
    let entry = dispatch::resolve(&group, "Pc.get", None).unwrap();
    assert!(matches!(
      entry.codecs().decode_input(PC, &[0xff; 11]),
      Err(CodecError::Decode(_))
    ));
    // A procedure without typed schemas refuses postcard (415), never falls back to JSON.
    let untyped = dispatch::resolve(&group, "Pc.untyped", None).unwrap();
    assert!(!untyped.codecs().supports(PC));
    assert!(untyped.codecs().supports(CodecId::Json));
    let err = decode_client_payload(untyped.codecs(), Some(&Postcard::encode(&1_i64).unwrap()))
      .unwrap_err();
    assert!(
      matches!(&err, CodecError::Unsupported(why) if why.contains("with_serde")),
      "{err}"
    );
  }

  #[test]
  fn postcard_oversized_frame_rejected_before_decode() {
    let too_big = vec![0_u8; MAX_FRAME_BYTES + 1];
    let expected = CodecError::TooLarge {
      size: MAX_FRAME_BYTES + 1,
      limit: MAX_FRAME_BYTES,
    };
    assert_eq!(decode_frame(&too_big).unwrap_err(), expected);
    assert_eq!(decode_exit(&too_big).unwrap_err(), expected);
    let group = group();
    let entry = dispatch::resolve(&group, "Pc.get", None).unwrap();
    assert_eq!(
      entry.codecs().decode_input(PC, &too_big).unwrap_err(),
      expected
    );
    // On a stream, the announced length is refused before its bytes are buffered.
    let header = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes();
    assert!(matches!(
      split_record(&header),
      Err(CodecError::TooLarge { .. })
    ));
    // The largest frame still decodes.
    let mut frame = Frame::new(id(1), FrameKind::Item).with_payload(Vec::new());
    let overhead = encode_frame(&frame).unwrap().len() + 2;
    frame.payload = Some(vec![7; MAX_FRAME_BYTES - overhead]);
    let bytes = encode_frame(&frame).unwrap();
    assert!(bytes.len() <= MAX_FRAME_BYTES);
    assert_eq!(decode_frame(&bytes).unwrap(), frame);
    let record = encode_record(&bytes).unwrap();
    assert_eq!(
      split_record(&record).unwrap(),
      Some((&bytes[..], record.len()))
    );
  }

  fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
      .worker_threads(2)
      .enable_all()
      .build()
      .unwrap()
  }

  proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// The same input sent as JSON and as postcard reaches the handler as the same value and
    /// gets the same outcome; a valid output encodes back to the same typed value.
    #[test]
    fn json_and_postcard_are_equivalent(name in "\\PC{0,12}", age in -10_i64..200) {
      let group = group();
      let entry = dispatch::resolve(&group, "Pc.person", None).unwrap();
      let json_value = json!({"name": name, "age": age});
      let json_bytes = serde_json::to_vec(&json_value).unwrap();
      let postcard_bytes = Postcard::encode(&(name.clone(), age)).unwrap();

      let from_json = entry.codecs().decode_input(CodecId::Json, &json_bytes).unwrap();
      let from_postcard = entry.codecs().decode_input(PC, &postcard_bytes).unwrap();
      prop_assert_eq!(&from_json, &json_value);
      prop_assert_eq!(&from_postcard, &json_value);

      let rt = runtime();
      let run = |value: Value| {
        rt.block_on(dispatch::start_unary(entry, value, ctx(), CallControl::none()).unwrap())
      };
      let via_json = run(from_json);
      let via_postcard = run(from_postcard);
      prop_assert_eq!(&via_json, &via_postcard);

      let valid = !name.is_empty() && (0..=150).contains(&age);
      let encoded = entry.codecs().encode_output(PC, &json_value);
      if valid {
        let back: (String, i64) = Postcard::decode(&encoded.unwrap()).unwrap();
        prop_assert_eq!(back, (name, age));
      } else {
        prop_assert!(matches!(encoded, Err(CodecError::Encode(_))));
      }
    }

    /// Arbitrary bytes never panic the frame decoder.
    #[test]
    fn decode_frame_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
      let _ = decode_frame(&bytes);
      let _ = decode_exit(&bytes);
      let _ = decode_result(&bytes);
      let _ = split_record(&bytes);
    }
  }
}
