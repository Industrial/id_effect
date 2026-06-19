#[::id_effect::capability(u32)]
struct Database;

fn main() {
  let _ = id_effect::require!(DatabaseKey);
}
