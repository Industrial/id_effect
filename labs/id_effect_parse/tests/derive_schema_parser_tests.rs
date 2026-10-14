use id_effect::Schema;
use id_effect_parse::SchemaBridge;

#[derive(Clone, Debug, PartialEq, Schema)]
struct User {
  name: String,
  age: i64,
}

#[test]
fn derive_schema_parser_round_trip() {
  let parser = SchemaBridge::parser_for::<User>();
  let user = parser
    .parse(r#"{"name":"Ada","age":36}"#.to_string())
    .unwrap()
    .0;
  assert_eq!(
    user,
    User {
      name: "Ada".into(),
      age: 36,
    }
  );

  let encoded = User::schema().encode(user);
  let decoded = User::schema().decode(encoded).unwrap();
  assert_eq!(decoded.name, "Ada");
}
