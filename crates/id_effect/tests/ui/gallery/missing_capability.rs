// Gallery: reading a capability (`~Mailer`) that is not in the effect's `caps!(..)`.
// Fix: add it to the signature, `caps!(Db, Mailer)`.
use id_effect::Never;
use id_effect::{Effect, caps, effect};

pub struct Db;

pub struct Mailer;

impl Mailer {
  pub fn send(&self, _to: &str) {}
}

fn welcome() -> Effect<(), Never, caps!(Db)> {
  effect!({
    let _db = ~Db;
    ~Mailer.send("new@user");
  })
}

fn main() {
  let _ = welcome();
}
