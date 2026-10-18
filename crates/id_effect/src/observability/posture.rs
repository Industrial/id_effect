//! The posture registry: every security opt-out the process has taken (ADR 0013 R10).
//!
//! Secure by default means the safe behaviour needs no code and every weakening has a name. Each
//! `dangerous_*` / `insecure_*` API in the workspace calls [`record_dangerous`] when the opt-out
//! is taken (a builder method, a constructor, a config switch), with a stable `id` (the API's
//! path, `"<crate>::<Type>::dangerous_<what>"`) and `reopens`, one sentence
//! naming the finding or risk the opt-out brings back. [`dangerous_opt_outs`] lists what was
//! recorded; the prod posture check reads it and refuses to start with an opt-out the operator has
//! not accepted.
//!
//! The registry is process-wide and records each id once: the first `reopens` text wins and later
//! calls with the same id are no-ops, so an opt-out taken per request still costs one map lookup,
//! not one entry. Nothing is allocated until the first call; a process that never opts out pays
//! nothing, and [`dangerous_opt_outs`] is then empty.
//!
//! ```
//! use id_effect::observability::posture::{dangerous_opt_outs, record_dangerous};
//!
//! record_dangerous("example::insecure_plaintext", "transport is neither encrypted nor authenticated");
//! record_dangerous("example::insecure_plaintext", "ignored: recorded once");
//! let found = dangerous_opt_outs();
//! assert!(found.contains(&("example::insecure_plaintext", "transport is neither encrypted nor authenticated")));
//! ```

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock, PoisonError};

static OPT_OUTS: OnceLock<Mutex<BTreeMap<&'static str, &'static str>>> = OnceLock::new();

/// Records that the process took the dangerous opt-out `id`, which reopens `reopens`.
///
/// The hook every `dangerous_*` / `insecure_*` API calls. Records once per `id`, process-wide;
/// the first `reopens` text is kept.
pub fn record_dangerous(id: &'static str, reopens: &'static str) {
  let map = OPT_OUTS.get_or_init(|| Mutex::new(BTreeMap::new()));
  map
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .entry(id)
    .or_insert(reopens);
}

/// The opt-outs recorded so far, as `(id, reopens)` pairs sorted by id (empty if none).
pub fn dangerous_opt_outs() -> Vec<(&'static str, &'static str)> {
  OPT_OUTS.get().map_or_else(Vec::new, |map| {
    map
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .map(|(id, reopens)| (*id, *reopens))
      .collect()
  })
}
