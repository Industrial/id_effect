#![allow(unused)]
struct Database;
struct Logger;

fn main() {
  let _ = id_effect::effect!(|r: &mut id_effect::caps!(Database)| {
    let _ = ~Logger;
  });
}
