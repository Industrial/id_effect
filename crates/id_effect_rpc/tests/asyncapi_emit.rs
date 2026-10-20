//! web.rpc #7: AsyncAPI 3.0 from the contract (r12-e12-4-openapi-asyncapi), validated offline
//! against the official AsyncAPI 3.0.0 JSON Schema.
//!
//! `fixtures/asyncapi-3.0.schema.json` is `schemas/3.0.0.json` of
//! <https://github.com/asyncapi/spec-json-schemas> at tag v6.11.1, unchanged (sha256
//! `d4571a420e6ffb7fcc7066c95a6db1202f299a3c51daa103d0706bf30f95e626`; treefmt keeps it
//! byte-for-byte). Copyright the AsyncAPI Initiative, licensed under the Apache License,
//! Version 2.0 (<https://github.com/asyncapi/spec-json-schemas/blob/master/LICENSE>). It bundles
//! every definition it references, the JSON Schema draft-07 meta-schema included, so no
//! reference is fetched.

use id_effect_rpc::asyncapi::{AsyncApiOptions, ChannelTopic, document, to_json};
use id_effect_rpc::contract::{
  Contract, ProcedureAccess, ProcedureContract, ProcedureKind, ServiceContract,
};
use serde_json::{Value, json};

const META_SCHEMA: &str = include_str!("fixtures/asyncapi-3.0.schema.json");

const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

fn fixture() -> Contract {
  Contract::from_json(include_str!("../rpc.contract.json")).expect("fixture contract")
}

/// A stream whose item type sits in `$defs`, deprecated, with a typed error.
fn with_defs() -> Contract {
  let item = json!({
    "$schema": DIALECT,
    "$ref": "#/$defs/chat::Message",
    "$defs": { "chat::Message": { "type": "object", "properties": { "text": { "type": "string" } } } },
  });
  let mut feed = ProcedureContract::new(
    "feed",
    ProcedureKind::ServerStream,
    ProcedureAccess::Guarded {
      policies: vec!["member".to_owned()],
    },
    json!({ "$schema": DIALECT, "type": "string" }),
    item,
  )
  .with_error(json!({ "oneOf": [{
    "type": "object",
    "properties": { "_tag": { "const": "Closed" } },
    "required": ["_tag"],
  }]}));
  feed.deprecated = true;
  Contract::new().with_service(ServiceContract::new("Chat", 1).with_procedure(feed))
}

fn topics() -> AsyncApiOptions {
  AsyncApiOptions::new("Todos events", "1.0.0")
    .with_description("Streams of the fixture services.")
    .with_host("api.example")
    .with_topic(
      ChannelTopic::new(
        "lobby",
        "Chat.join",
        json!({ "$schema": DIALECT, "$ref": "#/$defs/chat::Message", "$defs": {
          "chat::Message": { "type": "object", "properties": { "text": { "type": "string" } } },
        }}),
      )
      .with_description("Everyone."),
    )
    .with_topic(ChannelTopic::new(
      "news",
      "News.join",
      json!({ "type": "string" }),
    ))
}

fn validator() -> jsonschema::Validator {
  let meta: Value = serde_json::from_str(META_SCHEMA).expect("meta-schema is JSON");
  jsonschema::draft7::new(&meta).expect("the AsyncAPI 3.0 schema compiles without fetching")
}

fn assert_valid_asyncapi(doc: &Value) {
  let errors: Vec<String> = validator()
    .iter_errors(doc)
    .map(|e| format!("{e} at {}", e.instance_path()))
    .collect();
  assert!(
    errors.is_empty(),
    "invalid AsyncAPI 3.0 document: {errors:#?}"
  );
  let mut all = Vec::new();
  refs(doc, &mut all);
  assert!(!all.is_empty());
  for r in all {
    let pointer = r.strip_prefix('#').expect("local reference");
    assert!(doc.pointer(pointer).is_some(), "dangling {r}");
  }
}

fn refs(value: &Value, out: &mut Vec<String>) {
  match value {
    Value::Object(map) => {
      if let Some(Value::String(r)) = map.get("$ref") {
        out.push(r.clone());
      }
      map.values().for_each(|v| refs(v, out));
    }
    Value::Array(items) => items.iter().for_each(|v| refs(v, out)),
    _ => {}
  }
}

/// Whether `instance` validates against the schema at `pointer` in `doc`.
fn conforms(doc: &Value, pointer: &str, instance: &Value) -> bool {
  let mut root = doc.clone();
  root["$schema"] = json!(DIALECT);
  root["$ref"] = json!(format!("#{pointer}"));
  jsonschema::draft202012::new(&root)
    .expect("document schemas compile")
    .is_valid(instance)
}

#[test]
fn asyncapi_30_validates() {
  let doc = document(&fixture(), &topics());
  assert_eq!(doc["asyncapi"], "3.0.0");
  assert_eq!(doc["info"]["title"], "Todos events");
  assert_eq!(doc["defaultContentType"], "application/json");
  assert_eq!(doc["servers"]["ws"]["protocol"], "wss");
  assert_eq!(doc["servers"]["ws"]["pathname"], "/rpc/ws");
  assert_eq!(doc["servers"]["ws"]["host"], "api.example");
  assert_eq!(doc["servers"]["sse"]["protocol"], "https");
  assert_valid_asyncapi(&doc);
  assert_valid_asyncapi(&document(&with_defs(), &AsyncApiOptions::default()));
  assert_valid_asyncapi(&document(&Contract::new(), &AsyncApiOptions::default()));

  // The validator does reject a broken document.
  let mut broken = doc.clone();
  broken["operations"]["Todos-v1-upto.ws.send"]["action"] = json!("publish");
  assert!(!validator().is_valid(&broken));
  assert!(!validator().is_valid(&json!({ "asyncapi": "3.0.0" })));
}

#[test]
fn asyncapi_lists_every_stream_and_channel() {
  let contract = fixture();
  let doc = document(&contract, &topics());
  let channels = doc["channels"].as_object().unwrap();
  let operations = doc["operations"].as_object().unwrap();
  let mut expected = vec!["connection".to_owned()];
  for service in &contract.services {
    for p in &service.procedures {
      let base = format!("{}-v{}-{}", service.name, service.version, p.name);
      match p.kind {
        ProcedureKind::Query | ProcedureKind::Mutation => {
          assert!(!channels.contains_key(&format!("{base}.ws")), "{base}");
        }
        kind => {
          expected.push(format!("{base}.ws"));
          if matches!(
            kind,
            ProcedureKind::ServerStream | ProcedureKind::Subscription
          ) {
            expected.push(format!("{base}.sse"));
          }
        }
      }
    }
  }
  expected.extend(
    [
      "topic.lobby.sse",
      "topic.lobby.ws",
      "topic.news.sse",
      "topic.news.ws",
    ]
    .map(String::from),
  );
  expected.sort();
  let mut listed: Vec<String> = channels.keys().cloned().collect();
  listed.sort();
  assert_eq!(listed, expected);
  for (name, channel) in channels {
    let ops: Vec<&String> = operations
      .iter()
      .filter(|(_, op)| op["channel"]["$ref"] == json!(format!("#/channels/{name}")))
      .map(|(k, _)| k)
      .collect();
    assert!(!ops.is_empty(), "{name} has operations");
    assert!(
      channel["messages"]
        .as_object()
        .is_some_and(|m| !m.is_empty()),
      "{name}"
    );
  }

  // Kinds: what the server receives and sends.
  let messages = |channel: &str, op: &str| -> Vec<String> {
    operations[&format!("{channel}.{op}")]["messages"]
      .as_array()
      .unwrap()
      .iter()
      .map(|m| {
        m["$ref"]
          .as_str()
          .unwrap()
          .rsplit('/')
          .next()
          .unwrap()
          .to_owned()
      })
      .collect()
  };
  assert_eq!(
    messages("Todos-v1-upto.ws", "receive"),
    ["call", "cancel", "clientCredit"]
  );
  assert_eq!(messages("Todos-v1-upto.ws", "send"), ["serverItem", "exit"]);
  assert_eq!(
    messages("Todos-v1-sum.ws", "receive"),
    ["call", "cancel", "clientItem", "end"]
  );
  assert_eq!(
    messages("Todos-v1-sum.ws", "send"),
    ["exit", "serverCredit"]
  );
  assert_eq!(
    messages("Todos-v1-shout.ws", "receive"),
    ["call", "cancel", "clientItem", "end", "clientCredit"]
  );
  assert_eq!(
    messages("Todos-v1-shout.ws", "send"),
    ["serverItem", "exit", "serverCredit"]
  );
  assert_eq!(messages("Todos-v1-ticks.sse", "send"), ["item", "exit"]);
  assert_eq!(messages("connection", "send"), ["hello", "pong"]);
  assert_eq!(
    messages("topic.lobby.ws", "send"),
    ["joined", "message", "presence"]
  );
  assert_eq!(
    messages("topic.news.sse", "send"),
    ["joined", "message", "presence"]
  );

  // Bindings: the WebSocket handshake's subprotocols; SSE is a GET with `?input=`.
  let ws = &channels["Todos-v1-ticks.ws"];
  assert_eq!(ws["address"], "Todos.ticks");
  assert_eq!(
    ws["bindings"]["ws"]["headers"]["properties"]["Sec-WebSocket-Protocol"]["enum"],
    json!(["idrpc.json.v1", "idrpc.postcard.v1"])
  );
  assert_eq!(ws["x-rpc-kind"], "subscription");
  assert_eq!(
    channels["Todos-v1-ticks.sse"]["address"],
    "/rpc/Todos.ticks"
  );
  assert_eq!(
    operations["Todos-v1-ticks.sse.send"]["bindings"]["http"]["method"],
    "GET"
  );
  assert_eq!(channels["Todos-v1-sum.ws"]["x-rpc-max-items"], 100);
  // A topic is joined through its join procedure, the topic named in the input.
  assert_eq!(channels["topic.lobby.sse"]["address"], "/rpc/Chat.join");
  assert_eq!(channels["topic.lobby.ws"]["address"], "Chat.join");
  assert_eq!(
    channels["topic.lobby.ws"]["x-rpc-input"],
    json!({"topic": "lobby"})
  );
  assert_eq!(channels["topic.lobby.ws"]["description"], "Everyone.");
  let presence = &channels["topic.lobby.ws"]["messages"]["presence"]["payload"];
  assert_eq!(presence["properties"]["leaves"]["type"], "array");
  assert_eq!(
    presence["properties"]["joins"]["items"]["required"],
    json!(["key", "conn", "node", "meta"])
  );

  // The cursor: on subscription items and calls (WebSocket), and the SSE event id.
  let messages = &ws["messages"];
  assert_eq!(
    messages["serverItem"]["payload"]["properties"]["cursor"]["pattern"],
    "^c1\\.[A-Za-z0-9_-]{12}$"
  );
  assert!(messages["call"]["payload"]["properties"]["cursor"].is_object());
  let sse = &channels["Todos-v1-ticks.sse"]["messages"]["item"];
  assert!(
    sse["headers"]["properties"]["id"]["description"]
      .as_str()
      .unwrap()
      .contains("Last-Event-ID")
  );
  // Not part of the item schema.
  assert_eq!(sse["payload"], json!({"type": "integer"}));

  // Real frames conform.
  let item =
    json!({"id": 3, "kind": "item", "flags": 0, "cursor": "c1.AQAAAAAAAAAH", "payload": 7});
  assert!(conforms(
    &doc,
    "/channels/Todos-v1-ticks.ws/messages/serverItem/payload",
    &item
  ));
  let bad = json!({"id": 3, "kind": "item", "cursor": "nope", "payload": 7});
  assert!(!conforms(
    &doc,
    "/channels/Todos-v1-ticks.ws/messages/serverItem/payload",
    &bad
  ));
  let exit = json!({"id": 3, "kind": "exit", "flags": 0, "payload": {"_tag": "Failure", "error": {"_tag": "Locked"}}});
  assert!(conforms(
    &doc,
    "/channels/Todos-v1-upto.ws/messages/exit/payload",
    &exit
  ));
  let call = json!({"id": 3, "kind": "call", "tag": "Todos.upto", "v": 1, "payload": 5});
  assert!(conforms(
    &doc,
    "/channels/Todos-v1-upto.ws/messages/call/payload",
    &call
  ));
  let resume = json!({"id": 0, "kind": "resume", "payload": {"session": "AAAAAAAAAAAAAAAAAAAAAA", "calls": [{"id": 3, "cursor": "c1.AgAAAAAAAAAH"}]}});
  assert!(conforms(
    &doc,
    "/channels/connection/messages/resume/payload",
    &resume
  ));
  let union = &doc["components"]["schemas"]["Todos-v1-shout-Error"];
  assert_eq!(union["discriminator"], "_tag");
  assert!(
    union["oneOf"]
      .as_array()
      .unwrap()
      .iter()
      .any(|arm| arm["$ref"] == "#/components/schemas/error-ResumeExpired")
  );
}

#[test]
fn asyncapi_hoists_defs_and_marks_deprecated_and_guarded_streams() {
  let doc = document(
    &with_defs(),
    &topics()
      .with_path("/api")
      .with_ws_path("/live")
      .plain_transport(),
  );
  assert_eq!(doc["servers"]["ws"]["protocol"], "ws");
  assert_eq!(doc["servers"]["ws"]["pathname"], "/live");
  assert_eq!(doc["servers"]["sse"]["protocol"], "http");
  let schemas = &doc["components"]["schemas"];
  assert_eq!(schemas["Message"]["properties"]["text"]["type"], "string");
  let channel = &doc["channels"]["Chat-v1-feed.ws"];
  assert_eq!(
    channel["messages"]["serverItem"]["payload"]["properties"]["payload"],
    json!({"$ref": "#/components/schemas/Message"})
  );
  assert_eq!(
    channel["x-rpc-access"],
    json!({"guarded": {"policies": ["member"]}})
  );
  assert!(
    doc["operations"]["Chat-v1-feed.ws.send"]["description"]
      .as_str()
      .unwrap()
      .contains("Deprecated")
  );
  assert_eq!(
    doc["channels"]["Chat-v1-feed.sse"]["address"],
    "/api/Chat.feed"
  );
  // The topic's message body, hoisted under the same name.
  assert_eq!(
    doc["channels"]["topic.lobby.ws"]["messages"]["message"]["payload"]["properties"]["body"],
    json!({"$ref": "#/components/schemas/Message"})
  );
}

#[test]
fn asyncapi_output_is_deterministic() {
  let a = to_json(&fixture(), &topics());
  assert_eq!(a, to_json(&fixture(), &topics()));
  assert!(a.ends_with("}\n"));
  let reversed = fixture()
    .services
    .into_iter()
    .rev()
    .fold(Contract::new(), Contract::with_service);
  assert_eq!(to_json(&reversed, &topics()), a);
}
