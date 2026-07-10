#![allow(unused)]
struct Database;

fn main() {
  let _ = id_effect::effect!(|r: &mut id_effect::caps!(Database)| {
    let _ = ~Database();
  });
}
