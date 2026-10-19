// ADR 0013 R1 / R10, ADR 0014 (0-RTT replay): HTTP/3 0-RTT is off, and the one opt-out,
// `Http3Options::dangerous_allow_zero_rtt_for_queries`, admits early data for safe methods only
// (RPC queries). There is no way to enable 0-RTT for a mutation (POST): no such opt-out exists.
use id_effect_axum::server::h3::Http3Options;

fn main() {
  let _ = Http3Options::new().dangerous_allow_zero_rtt_for_mutations();
}
