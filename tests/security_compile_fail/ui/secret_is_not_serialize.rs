// ADR 0013 R2 (M13): a `Secret` is read from configuration but never written out: it has no
// `Serialize`, so it cannot leak into a JSON response or a dumped config.
use id_effect_kit::config::SecretString;

fn main() {
  let token = SecretString::new("sk-live-42".to_string());
  let _ = serde_json::to_string(&token);
}
