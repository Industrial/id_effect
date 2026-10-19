// Design note §4: a user error variant may not claim a reserved failure tag.
use id_effect_rpc as rpc;

#[derive(Debug, rpc::RpcErrorUnion)]
enum TodoError {
  NotFound { id: i64 },
  InternalError { id: String },
}

fn main() {}
