use id_effect_dioxus::{RealtimeEvent, RealtimeHub, decode_form, require_field};
use std::sync::Arc;

#[test]
fn decode_urlencoded_form() {
  let form = decode_form("name=alice&score=10").expect("decode");
  assert_eq!(require_field(&form, "name").unwrap(), "alice");
}

#[test]
fn hub_publishes_events() {
  let hub = Arc::new(RealtimeHub::new(8));
  let mut rx = hub.subscribe();
  hub.publish(RealtimeEvent {
    topic: "t".into(),
    event: "ping".into(),
    data_json: "{}".into(),
  });
  let ev = rx.try_recv().expect("event");
  assert_eq!(ev.event, "ping");
}
