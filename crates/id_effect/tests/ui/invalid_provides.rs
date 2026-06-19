#[derive(::id_effect::ProviderSpecDerive)]
struct BadLive;

impl BadLive {
  fn new() -> u32 {
    1
  }
}

fn main() {}
