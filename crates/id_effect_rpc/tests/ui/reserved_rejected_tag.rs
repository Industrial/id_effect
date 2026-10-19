// Design note §4: `Rejected` (a frame transport's refusal before a call ran) is reserved too.
use id_effect_rpc as rpc;

#[derive(Debug, rpc::RpcErrorUnion)]
enum TodoError {
  NotFound {
    id: i64,
  },
  #[rpc(tag = "Rejected")]
  Refused,
}

fn main() {}
