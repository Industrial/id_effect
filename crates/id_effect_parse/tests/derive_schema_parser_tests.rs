use id_effect::SchemaParser;

#[derive(Clone, Debug, PartialEq, SchemaParser)]
struct User {
  name: String,
  age: i64,
}

#[test]
fn derive_schema_parser_round_trip() {
  let parser = User::parser();
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
