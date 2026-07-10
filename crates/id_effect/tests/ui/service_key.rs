fn main() {
  // `service_key!` was removed in v3; use `#[capability]` instead.
  id_effect::service_key!(struct PublicDb);
}
