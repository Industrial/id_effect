//! Helpers shared by the fuzz targets in `fuzz_targets/` (see `docs/security/fuzzing.md`).
//!
//! The targets reach the crates only through their public API; this library holds nothing but
//! input shaping, the schemas the route targets decode into, and a blocking executor.

use std::collections::BTreeMap;
use std::future::Future;

use id_effect::schema::{self, HasSchema, Schema};

/// Run `future` to completion on the current thread (the extractors and the router need no
/// tokio runtime).
pub fn block_on<F: Future>(future: F) -> F::Output {
  futures::executor::block_on(future)
}

/// `bytes` as text a URI accepts: printable ASCII other than `#` and `?` is kept as it is (so
/// `%`, `&`, `=` and `/` keep their meaning and malformed escapes still reach the decoder),
/// every other byte becomes a `%XX` escape.
pub fn uri_text(bytes: &[u8]) -> String {
  let mut out = String::with_capacity(bytes.len());
  for &b in bytes {
    if b.is_ascii_graphic() && b != b'#' && b != b'?' {
      out.push(char::from(b));
    } else {
      out.push_str(&format!("%{b:02X}"));
    }
  }
  out
}

/// `data` split at the first `\n---\n` (the separator the seeds use between two documents);
/// the second part is empty when there is none.
pub fn split_two(data: &[u8]) -> (&[u8], &[u8]) {
  const SEP: &[u8] = b"\n---\n";
  match data.windows(SEP.len()).position(|w| w == SEP) {
    Some(at) => (&data[..at], &data[at + SEP.len()..]),
    None => (data, &[]),
  }
}

/// A search query or form: an integer in range, an optional number, a repeated string and an
/// optional boolean (every coercion the string decoder has).
pub struct Search;

/// [`Search`]'s decoded value.
pub type SearchValue = (i64, Option<f64>, Vec<String>, Option<bool>);

impl HasSchema for Search {
  type A = SearchValue;
  type I = SearchValue;
  type E = ();

  fn schema() -> Schema<SearchValue, SearchValue> {
    schema::struct4(
      "page",
      schema::range(schema::i64(), 1, 1000),
      "score",
      schema::optional(schema::f64()),
      "tag",
      schema::array(schema::length(schema::string(), 1, 16)),
      "exact",
      schema::optional(schema::bool_()),
    )
  }
}

/// Any string keys and values (a record schema).
pub struct Labels;

impl HasSchema for Labels {
  type A = BTreeMap<String, String>;
  type I = BTreeMap<String, String>;
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::record(schema::string())
  }
}
