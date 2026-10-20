//! `Schema` decode from JSON (id_effect, feature `schema-serde`): bytes → `serde_json::Value` →
//! `unknown_from_serde_json` → `decode_unknown` / `decode_unknown_all` over a schema using the
//! struct, optional, array, record and validator combinators.
//!
//! Checks the documented contract besides "no panic": `decode_unknown_all` succeeds exactly when
//! `decode_unknown` does, with the same value, and reports at least one issue when it fails; a
//! decoded value encodes and decodes back to itself.
#![no_main]

use std::sync::LazyLock;

use id_effect::schema::{self, Unknown, unknown_from_serde_json};
use libfuzzer_sys::fuzz_target;

type Check = Box<dyn Fn(&Unknown) + Send + Sync>;

static CHECK: LazyLock<Check> = LazyLock::new(|| {
  let meta = schema::struct_(
    "url",
    schema::url(schema::string()),
    "weight",
    schema::range(schema::f64(), -1.0e9, 1.0e9),
  );
  let s = schema::struct6(
    "id",
    schema::uuid(schema::string::<()>()),
    "name",
    schema::length(schema::string(), 1, 64),
    "email",
    schema::optional(schema::email(schema::string())),
    "age",
    schema::range(schema::i64(), 0, 150),
    "tags",
    schema::max_items(
      schema::array(schema::one_of(schema::string(), &["a", "b", "c"])),
      8,
    ),
    "meta",
    schema::record(meta),
  );
  Box::new(move |input: &Unknown| {
    let first = s.decode_unknown(input);
    let all = s.decode_unknown_all(input);
    match (&first, &all) {
      (Ok(a), Ok(b)) => {
        assert_eq!(a, b, "decode_unknown and decode_unknown_all disagree");
        let again = s
          .decode(s.encode(a.clone()))
          .expect("an encoded value decodes");
        assert_eq!(&again, a, "encode/decode round trip changed the value");
      }
      (Err(_), Err(errors)) => assert!(!errors.issues.is_empty(), "failure without an issue"),
      _ => panic!("decode_unknown {first:?} but decode_unknown_all {all:?}"),
    }
  })
});

fuzz_target!(|data: &[u8]| {
  let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) else {
    return;
  };
  (*CHECK)(&unknown_from_serde_json(value));
});
