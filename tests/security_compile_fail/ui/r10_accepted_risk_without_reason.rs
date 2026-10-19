// ADR 0013 R10: an operator accepts an opt-out for a stated reason; there is no way to accept
// one without it.
use id_effect_kit::posture::AcceptedRisks;

fn main() {
  let _ = AcceptedRisks::new().accept("id_effect_sql::RawSqlCapability::layer_dangerous");
}
