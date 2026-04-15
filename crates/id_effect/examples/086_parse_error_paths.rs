//! Ex 086 — `ParseError::prefix` builds dot paths for nested fields.
use id_effect::schema::ParseError;

fn main() {
  let e = ParseError::new("age", "bad").prefix("user");
  assert_eq!(e.path, "user.age");
  println!("086_parse_error_paths ok");
}
