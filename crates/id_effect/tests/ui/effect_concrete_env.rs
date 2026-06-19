fn main() {
  // Effect::provide was removed in v3; use run_with instead.
  let _: fn(id_effect::Effect<(), (), ()>) = id_effect::Effect::provide;
}
