//! `#[derive(Schema)]` and the `effect!` do-notation macro, used through their `id_effect`
//! re-exports (the normal way to consume this crate).
//!
//! ```bash
//! cargo run -p id_effect_proc_macro --example derive_schema
//! ```

use id_effect::schema::{HasSchema, Unknown};
use id_effect::{Effect, Schema, effect, succeed};

#[derive(Clone, Debug, PartialEq, Schema)]
struct User {
  #[schema("user_name")]
  name: String,
  age: i64,
}

fn bump(age: i64) -> Effect<i64, String> {
  succeed(age + 1)
}

fn older(user: User) -> Effect<User, String> {
  effect!({
    let age = ~bump(user.age);
    User { age, ..user }
  })
}

fn main() {
  let wire = User::schema().encode(User {
    name: "Ada".into(),
    age: 36,
  });
  let Unknown::Object(fields) = &wire else {
    unreachable!("structs encode as objects");
  };
  println!("wire fields: {:?}", fields.keys().collect::<Vec<_>>());

  let decoded = <User as HasSchema>::schema()
    .decode_unknown(&wire)
    .expect("round trip");
  let birthday = older(decoded)
    .block_on()
    .into_result()
    .expect("pure effect");
  println!("{birthday:?}");
}
