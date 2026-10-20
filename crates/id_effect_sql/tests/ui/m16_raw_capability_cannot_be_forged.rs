// M16: `RawSqlCapability` has no public constructor; only `RawSqlCapability::layer_dangerous()`
// (which records a posture opt-out) provides it.
use id_effect_sql::RawSqlCapability;

fn main() {
  let _forged = RawSqlCapability { _private: () };
}
