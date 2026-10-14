//! `#[derive(Schema)]` and the `effect!` do-notation macro, used through their `id_effect`
//! re-exports (the normal way to consume this crate).
//!
//! ```bash
//! cargo run -p id_effect_proc_macro --example derive_schema
//! ```

use id_effect::schema::{HasSchema, Unknown};
use id_effect::{Effect, Schema, effect, run_blocking, succeed};

#[derive(Clone, Debug, PartialEq, Schema)]
struct User {
  #[schema("user_name")]
  name: String,
  age: i64,
}

fn older(user: User) -> Effect<User, String, ()> {
  effect! {
    let age = ~succeed::<i64, String, ()>(user.age + 1);
    User { age, ..user }
  }
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
  let birthday = run_blocking(older(decoded), ()).expect("pure effect");
  println!("{birthday:?}");
}
