//! Lens composition over nested records.

use id_effect_optics::{Lens, field};

#[derive(Clone, Debug)]
struct Address {
  city: String,
}

#[derive(Clone, Debug)]
struct Person {
  name: String,
  address: Address,
}

fn main() {
  let city = field(
    |p: &Person| &p.address,
    |mut p, address| {
      p.address = address;
      p
    },
  )
  .compose(field(
    |a: &Address| &a.city,
    |mut a, city| {
      a.city = city;
      a
    },
  ));

  let person = Person {
    name: "Ada".into(),
    address: Address {
      city: "London".into(),
    },
  };

  let updated = city.modify(person, |c| format!("{c}, UK"));
  println!("city = {}", updated.address.city);
}
