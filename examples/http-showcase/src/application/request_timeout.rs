//! [`RequestTimeout`]: a store call ran past the request timeout.

use std::fmt;

use id_effect::EffectData;

/// The store call (retries included) did not finish within
/// [`RequestPolicy::timeout`](super::RequestPolicy::timeout); its fiber was interrupted.
#[derive(Clone, Copy, Debug, EffectData)]
pub struct RequestTimeout;

impl fmt::Display for RequestTimeout {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("the request timed out")
  }
}

impl std::error::Error for RequestTimeout {}
