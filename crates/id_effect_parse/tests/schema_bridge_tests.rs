use id_effect::schema::i64;
use id_effect_parse::SchemaBridgeStub;

#[test]
fn schema_bridge_is_stub() {
  let schema = i64::<()>();
  assert!(SchemaBridgeStub::parser_for(&schema).is_none());
}
