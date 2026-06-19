//! Schema paths and JSON Patch on Unknown documents.

use id_effect::schema::Unknown;
use id_effect_optics::{PatchOp, apply_patch, create_at_path, get_at_path, object};

fn main() {
  let doc = object([("count", Unknown::I64(1))]);
  let nested = create_at_path(doc, "user.name", Unknown::String("Ada".into())).unwrap();
  let patched = apply_patch(
    nested,
    &PatchOp::Replace {
      path: "count".into(),
      value: Unknown::I64(2),
    },
  )
  .unwrap();
  println!("name = {:?}", get_at_path(&patched, "user.name").unwrap());
}
