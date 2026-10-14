//! JSON text parsing with `#[derive(Schema)]` + [`SchemaBridge::parser_for`].

use id_effect::Schema;
use id_effect_parse::SchemaBridge;

#[derive(Clone, Debug, Schema)]
struct User {
  name: String,
  age: i64,
}

fn main() {
  let parser = SchemaBridge::parser_for::<User>();
  let (user, _) = parser
    .parse(r#"{"name":"Ada","age":36}"#.to_string())
    .expect("parse user");
  println!("user = {user:?}");
}
