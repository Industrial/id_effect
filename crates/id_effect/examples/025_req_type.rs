//! Ex 025 — `req!` names the environment type for services.
use id_effect::{Get, ThereHere, ctx, effect, req, run_blocking, service_key};

service_key!(struct HostKey);
service_key!(struct PortKey);

type Env = req!(HostKey: &'static str | PortKey: u16);

fn main() {
  let read = effect!(|r: &mut Env| {
    let host = ~Ok::<_, ()>(*Get::<HostKey>::get(r));
    let port = ~Ok::<_, ()>(*r.get_path::<PortKey, ThereHere>());
    format!("{host}:{port}")
  });
  let env = ctx!(HostKey => "127.0.0.1", PortKey => 9000_u16);
  assert_eq!(
    run_blocking(read, env),
    Ok::<String, ()>("127.0.0.1:9000".to_owned())
  );
  println!("025_req_type ok");
}
