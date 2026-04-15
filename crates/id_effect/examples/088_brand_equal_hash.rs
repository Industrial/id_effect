//! Ex 088 — `Brand` is distinct at the type level; `Equal` / `EffectHash` still apply.
use id_effect::Brand;
use id_effect::schema::equal::{equals, hash};

struct UsdTag;
type Usd = Brand<i64, UsdTag>;

fn main() {
  let a = Usd::nominal(100);
  let b = Usd::nominal(100);
  assert!(equals(&a, &b));
  assert_eq!(hash(&a), hash(&b));
  println!("088_brand_equal_hash ok");
}
