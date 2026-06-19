#![allow(unused)]

#[::id_effect::capability(u32)]
struct Database;

fn main() {
  let _ = id_effect::effect!(|r: &mut id_effect::caps!(DatabaseKey)| {
    let _ = ~DatabaseKey();
  });
}
