// (2a) The same capability type provided twice by one env struct (caught by the derive).
use kernel_b::Env;
use spike_corpus::Db;

#[derive(Clone, Env)]
struct AppEnv {
  primary: Db,
  replica: Db,
}

fn main() {}
