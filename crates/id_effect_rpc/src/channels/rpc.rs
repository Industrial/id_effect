//! Topics as RPC subscriptions: a join is a `subscription` procedure call, so it is served over
//! every transport that serves subscriptions (WebSocket, SSE, later WebTransport) and every
//! join runs the procedure's guards and then the [`JoinPolicy`](super::JoinPolicy).

use id_effect::{DynEnv, Effect};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ChannelError;
use super::hub::{ChannelEvent, ChannelMessage, Channels, JoinSpec, Membership};
use super::policy::principal_of;
use crate::handler::{SubscriptionHandler, WireSchema};
use crate::registry::{Decided, Procedure, RpcGroup};
use crate::wire::{Cursor, InputIssue};

/// The connection a call arrived on, as its transport puts it in the call's environment. The
/// cap on topics per connection counts by it; a call without one is its own connection.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub String);

/// The input of a join procedure: `{"topic": "room:general", "meta": {"status": "online"}}`
/// (`meta` optional).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinInput {
  /// The topic to join.
  pub topic: String,
  /// The metadata to join with.
  #[serde(default)]
  pub meta: Value,
}

impl JoinInput {
  /// The input's wire schema (JSON).
  pub fn wire_schema() -> WireSchema<Self> {
    WireSchema::from_fns(
      |value| {
        serde_json::from_value(value.clone()).map_err(|e| {
          vec![InputIssue {
            path: String::new(),
            message: e.to_string(),
          }]
        })
      },
      |input| serde_json::to_value(input).map_err(|e| e.to_string()),
      json!({
        "type": "object",
        "properties": {
          "topic": {"type": "string", "minLength": 1, "maxLength": super::MAX_TOPIC_LEN},
          "meta": {}
        },
        "required": ["topic"],
        "additionalProperties": false
      }),
    )
  }
}

impl<M: ChannelMessage> ChannelEvent<M> {
  /// The item's wire schema (JSON; the message body is any JSON value).
  pub fn wire_schema() -> WireSchema<Self> {
    let presence = json!({
      "type": "object",
      "properties": {
        "key": {"type": "string"},
        "conn": {"type": "string"},
        "node": {"type": "string"},
        "meta": {}
      },
      "required": ["key", "conn", "node", "meta"]
    });
    let list = json!({"type": "array", "items": presence});
    WireSchema::from_fns(
      |value| {
        serde_json::from_value(value.clone()).map_err(|e| {
          vec![InputIssue {
            path: String::new(),
            message: e.to_string(),
          }]
        })
      },
      |event| serde_json::to_value(event).map_err(|e| e.to_string()),
      json!({
        "oneOf": [
          {
            "type": "object",
            "properties": {
              "type": {"const": "joined"},
              "topic": {"type": "string"},
              "me": presence,
              "members": list
            },
            "required": ["type", "topic", "me", "members"]
          },
          {
            "type": "object",
            "properties": {
              "type": {"const": "message"},
              "topic": {"type": "string"},
              "node": {"type": "string"},
              "from": {"type": ["string", "null"]},
              "body": {}
            },
            "required": ["type", "topic", "node", "body"]
          },
          {
            "type": "object",
            "properties": {
              "type": {"const": "presence"},
              "topic": {"type": "string"},
              "joins": list,
              "leaves": list
            },
            "required": ["type", "topic", "joins", "leaves"]
          }
        ]
      }),
    )
  }
}

impl<M: ChannelMessage> Channels<M> {
  /// The handler of a join procedure: the principal is the call environment's
  /// `CurrentPrincipal` (anonymous without one), the connection its [`ConnectionId`]. Channels
  /// are live, so a resume cursor is ignored: a resumed call is a fresh join (and is authorized
  /// again).
  pub fn join_handler(&self) -> impl SubscriptionHandler<JoinInput, ChannelEvent<M>, ChannelError> {
    let channels = self.clone();
    move |input: JoinInput, _after: Option<Cursor>, env: &mut DynEnv| {
      let principal = principal_of(env);
      let conn = env
        .try_get::<ConnectionId>()
        .map_or_else(|_| channels.fresh_conn(), |c| c.0.clone());
      let spec = JoinSpec::new(input.topic, conn).with_meta(input.meta);
      let joined: Effect<Membership<M>, ChannelError, DynEnv> = channels.join(spec, principal);
      joined.map(Membership::into_subscription)
    }
  }

  /// Register the join procedure on `group`: `procedure` (a `subscription` with its access
  /// decision, e.g. `Procedure::subscription("Chat.join").guard(authenticated)`), input
  /// [`JoinInput`], items [`ChannelEvent`].
  pub fn serve_joins<D: Decided>(&self, group: &mut RpcGroup, procedure: Procedure<D>) {
    group.subscription(
      procedure,
      JoinInput::wire_schema(),
      ChannelEvent::<M>::wire_schema(),
      self.join_handler(),
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn join_input_decodes_and_refuses_unknown_fields() {
    let schema = JoinInput::wire_schema();
    let input = schema.decode(&json!({"topic": "t"})).unwrap();
    assert_eq!(input.meta, Value::Null);
    assert!(schema.decode(&json!({"topic": "t", "x": 1})).is_err());
    assert!(schema.json_schema()["required"].is_array());
    assert_eq!(
      schema.encode(input).unwrap(),
      json!({"topic": "t", "meta": null})
    );
  }

  #[test]
  fn event_schema_round_trips() {
    let schema = ChannelEvent::<String>::wire_schema();
    let event = ChannelEvent::Message {
      topic: "t".into(),
      node: "a".into(),
      from: None,
      body: "hi".to_string(),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "message");
    assert_eq!(schema.decode(&value).unwrap(), event);
    assert_eq!(schema.encode(event).unwrap(), value);
    assert!(schema.decode(&json!({"type": "nope"})).is_err());
    assert_eq!(schema.json_schema()["oneOf"].as_array().unwrap().len(), 3);
  }
}
