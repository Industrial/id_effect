// ADR 0013 R5 + R10 (M16): the raw-SQL capability is minted only by the recorded opt-out
// `RawSqlCapability::layer_dangerous`; it cannot be built by hand.
use id_effect_kit::sql::RawSqlCapability;

fn main() {
  let _ = RawSqlCapability { _private: () };
}
