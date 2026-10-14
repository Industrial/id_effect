fn main() {
  // `service_key!` was removed in 0.3; use the service type directly with `caps!` / `#[provides]`.
  id_effect::service_key!(struct PublicDb);
}
