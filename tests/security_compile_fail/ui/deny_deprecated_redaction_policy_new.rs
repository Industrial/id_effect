// ADR 0013 R3 (M11): `RedactionPolicy::new()` redacts nothing; it is deprecated for
// `recommended()` (the pipeline default) or the explicit `empty()`, so `deny(deprecated)` builds
// reject it.
#![deny(deprecated)]

use id_effect_kit::logger::RedactionPolicy;

fn main() {
  let _ = RedactionPolicy::new();
}
