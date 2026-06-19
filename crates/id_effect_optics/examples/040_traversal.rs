//! Traversals over vectors and optional fields.

use id_effect_optics::{at_option, at_vec, field, vector_each};

#[derive(Clone)]
struct Row {
  values: Vec<i32>,
}

#[derive(Clone)]
struct Profile {
  nickname: Option<String>,
}

fn main() {
  let doubled = vector_each::<i32>().over(vec![1, 2, 3], |n| n * 2);
  let row = at_vec(field(
    |r: &Row| &r.values,
    |mut r, values| {
      r.values = values;
      r
    },
  ))
  .over(Row { values: vec![1, 2] }, |n| n + 10);
  let profile = at_option(field(
    |p: &Profile| &p.nickname,
    |mut p, nickname| {
      p.nickname = nickname;
      p
    },
  ))
  .over(
    Profile {
      nickname: Some("ada".into()),
    },
    |n| n.to_uppercase(),
  );
  println!("{doubled:?} {:?} {:?}", row.values, profile.nickname);
}
