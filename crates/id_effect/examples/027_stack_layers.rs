//! Ex 027 — `Stack` composes layers into an HList.
use id_effect::{Cons, Layer, LayerFn, Nil, Stack, Tagged, service_key};

service_key!(struct AKey);
service_key!(struct BKey);

fn main() {
  let stack = Stack(
    LayerFn(|| Ok::<Tagged<AKey, u8>, ()>(Tagged::<AKey, _>::new(1_u8))),
    LayerFn(|| Ok::<Tagged<BKey, u16>, ()>(Tagged::<BKey, _>::new(2_u16))),
  );
  let Cons(a, Cons(b, Nil)) = stack.build().expect("stack");
  assert_eq!(a.value, 1);
  assert_eq!(b.value, 2);
  println!("027_stack_layers ok");
}
