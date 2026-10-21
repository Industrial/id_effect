//! The Pipeline reference app: a multi-tenant document-intake pipeline and its back office.
//!
//! Documents arrive for a tenant and move through [`Stage::ALL`] in order: ingest, AI extraction
//! into a typed record, classification by a bounded tool loop, embedding-based near-duplicate
//! detection, human review in the generated admin and an htmx review queue (English or German),
//! and publication through an outbox event. 0.13 builds the app in steps (release spec 0.13.0,
//! decision D1); this first step fixes the stage model.
//!
//! The name is the app's, not `id_effect_resilience::Pipeline` (a policy type): the docs always
//! say "the Pipeline reference app".
//!
//! ```
//! use pipeline::{Stage, stages};
//!
//! let all = stages().block_on().into_result().unwrap();
//! assert_eq!(all.first(), Some(&Stage::Ingest));
//! assert_eq!(Stage::Review.next(), Some(Stage::Publish));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use id_effect_kit::id_effect::{Effect, succeed};
use std::fmt;

/// Compiles and runs the README snippet as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// One stage a document passes through, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
  /// A document arrives for a tenant (RPC mutation or upload); one job per stage follows.
  Ingest,
  /// An AI model turns the text into a typed extraction (structured output).
  Extract,
  /// A bounded tool loop attaches labels.
  Classify,
  /// Embeddings flag near-duplicates within the tenant.
  Embed,
  /// Reviewers accept or correct the document in the back office.
  Review,
  /// A lifecycle hook writes `DocumentPublished` to the outbox.
  Publish,
}

impl Stage {
  /// Every stage, in pipeline order.
  pub const ALL: [Self; 6] = [
    Self::Ingest,
    Self::Extract,
    Self::Classify,
    Self::Embed,
    Self::Review,
    Self::Publish,
  ];

  /// The stage after this one, or `None` after [`Stage::Publish`].
  pub fn next(self) -> Option<Self> {
    Self::ALL
      .iter()
      .position(|s| *s == self)
      .and_then(|i| Self::ALL.get(i + 1).copied())
  }

  /// The stage's name (`ingest`, `extract`, …), used as its job queue name.
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Ingest => "ingest",
      Self::Extract => "extract",
      Self::Classify => "classify",
      Self::Embed => "embed",
      Self::Review => "review",
      Self::Publish => "publish",
    }
  }
}

impl fmt::Display for Stage {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The pipeline's stages, as an effect (the app wires one job queue per stage from it).
pub fn stages() -> Effect<Vec<Stage>> {
  succeed(Stage::ALL.to_vec())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn stages_chain_in_order_and_end_at_publish() {
    let mut stage = Stage::Ingest;
    let mut seen = vec![stage];
    while let Some(next) = stage.next() {
      seen.push(next);
      stage = next;
    }
    assert_eq!(seen, Stage::ALL);
    let names: Vec<String> = seen.iter().map(ToString::to_string).collect();
    assert_eq!(
      names,
      [
        "ingest", "extract", "classify", "embed", "review", "publish"
      ]
    );
  }
}
