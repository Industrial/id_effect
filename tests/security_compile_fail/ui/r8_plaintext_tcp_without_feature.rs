// ADR 0013 R8: the mesh's TCP transport is TLS; plaintext needs the `insecure-plaintext-tcp`
// feature of id_effect_node, which the kit's `cluster` does not turn on.
use id_effect_kit::node::TcpTransport;

fn main() {
  let _ = TcpTransport::dangerous_plaintext();
}
