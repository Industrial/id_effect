//! JSON text parsing with `#[derive(SchemaParser)]`.

use id_effect::SchemaParser;

#[derive(Clone, Debug, SchemaParser)]
struct User {
  name: String,
  age: i64,
}

fn main() {
  let parser = User::parser();
  let (user, _) = parser
    .parse(r#"{"name":"Ada","age":36}"#.to_string())
    .expect("parse user");
  println!("user = {user:?}");
}
