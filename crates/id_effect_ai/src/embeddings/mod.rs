//! Embeddings (`feature = "embeddings"`, ai.llm #5): the provider-neutral [`Embedder`]
//! capability, bounded batches, typed dimension checks and in-app similarity.
//!
//! - **One shape for every provider**: an [`Embedder`] answers an [`EmbedRequest`] (a model
//!   and its input texts) with an [`EmbedResponse`]: one [`Embedding`] per input, in input
//!   order, and the token [`Usage`]. The adapters are `vendors::openai::embed::OpenAiEmbedder`
//!   (features `openai` + `embeddings`) and `vendors::ollama::embed::OllamaEmbedder` (features
//!   `ollama` + `embeddings`); tests use `testing::ScriptedModel` (feature `test-util`), whose
//!   embeddings are deterministic.
//! - **Bounded batches**: [`Embedder::embed`] refuses more than
//!   [`EmbeddingLimits::max_inputs`] inputs and splits the rest into vendor calls of at most
//!   [`EmbeddingLimits::max_batch_size`] inputs, run in order; the results are concatenated
//!   in input order and their usage summed.
//! - **Dimension check**: every vector of a response (across all its batches) must have the
//!   same length, equal to [`EmbedRequest::dimensions`] when set; a mismatch is
//!   [`EmbeddingError::DimensionMismatch`], never a silently truncated comparison. The
//!   similarity helpers ([`cosine_similarity`], [`dot_product`], [`top_k_similar`]) check it
//!   too.
//! - **Retries, spans, usage** as for chat: transient statuses are retried with backoff, and
//!   every vendor call runs in an `ai.request` span (`ai.operation = "embed"`) annotated with
//!   `ai.usage.input_tokens`, `ai.embedding.inputs` and `ai.embedding.dimensions`.
//!
//! Secure by default (ADR 0013): the adapters send through id_effect_platform's client (SSRF
//! policy, timeouts, body caps), API keys are `Secret`s, and **input text is never logged**:
//! not on spans (even under `AiConfig::dangerous_log_prompts`), not in `Debug` of an
//! [`EmbedRequest`], not in validation errors.
//!
//! ```
//! use id_effect_ai::embeddings::{Embedding, EmbeddingError, cosine_similarity, top_k_similar};
//!
//! let query = Embedding::new(vec![1.0, 0.0]);
//! let docs = [Embedding::new(vec![0.0, 1.0]), Embedding::new(vec![0.9, 0.1])];
//! let best = top_k_similar(&query, &docs, 1).expect("same dimensions");
//! assert_eq!(best[0].0, 1);
//! assert_eq!(
//!   cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]),
//!   Err(EmbeddingError::DimensionMismatch { expected: 2, actual: 3 })
//! );
//! ```

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, fail, hlist, need, succeed};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::chat::Usage;
use crate::error::AiError;
use crate::http_util::from_result;

#[cfg(feature = "test-util")]
mod scripted;
mod similarity;
#[cfg(any(feature = "openai", feature = "ollama"))]
mod telemetry;

pub use similarity::{cosine_similarity, dot_product, top_k_similar};
#[cfg(any(feature = "openai", feature = "ollama"))]
pub(crate) use telemetry::observe_embed;

/// One embedding vector. Every provider's adapter returns this shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Embedding {
  vector: Vec<f32>,
}

impl Embedding {
  /// An embedding of `vector`.
  #[inline]
  pub fn new(vector: Vec<f32>) -> Self {
    Self { vector }
  }

  /// The components.
  #[inline]
  pub fn as_slice(&self) -> &[f32] {
    &self.vector
  }

  /// The components, by value.
  #[inline]
  pub fn into_vec(self) -> Vec<f32> {
    self.vector
  }

  /// The number of components.
  #[inline]
  pub fn dimensions(&self) -> usize {
    self.vector.len()
  }

  /// The Euclidean (L2) norm.
  pub fn norm(&self) -> f32 {
    similarity::norm(&self.vector)
  }

  /// This vector scaled to norm 1 (a zero vector stays zero).
  #[must_use]
  pub fn normalized(self) -> Self {
    let norm = self.norm();
    if norm == 0.0 {
      return self;
    }
    Self::new(self.vector.into_iter().map(|x| x / norm).collect())
  }

  /// The dot product with `other` ([`dot_product`]).
  pub fn dot(&self, other: &Self) -> Result<f32, EmbeddingError> {
    dot_product(&self.vector, &other.vector)
  }

  /// The cosine similarity with `other` ([`cosine_similarity`]).
  pub fn cosine(&self, other: &Self) -> Result<f32, EmbeddingError> {
    cosine_similarity(&self.vector, &other.vector)
  }
}

impl From<Vec<f32>> for Embedding {
  fn from(vector: Vec<f32>) -> Self {
    Self::new(vector)
  }
}

/// A request to embed `inputs` with `model`. Build it with [`EmbedRequest::new`] or
/// [`EmbedRequest::one`] (non-exhaustive). `Debug` prints the number of inputs, never their
/// text.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EmbedRequest {
  /// Model id (vendor-specific string).
  pub model: String,
  /// The texts to embed, one vector each.
  pub inputs: Vec<String>,
  /// The expected vector length. When set it is sent to the vendor (OpenAI `text-embedding-3`
  /// and Ollama shorten their vectors to it) and every returned vector must have it; when
  /// unset, the first vector's length is the one every other must match.
  #[serde(default)]
  pub dimensions: Option<usize>,
}

impl fmt::Debug for EmbedRequest {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("EmbedRequest")
      .field("model", &self.model)
      .field(
        "inputs",
        &format_args!("<{} inputs redacted>", self.inputs.len()),
      )
      .field("dimensions", &self.dimensions)
      .finish()
  }
}

impl EmbedRequest {
  /// A request to embed every text of `inputs` with `model`.
  pub fn new<I, S>(model: impl Into<String>, inputs: I) -> Self
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    Self {
      model: model.into(),
      inputs: inputs.into_iter().map(Into::into).collect(),
      dimensions: None,
    }
  }

  /// A request to embed one `text`.
  pub fn one(model: impl Into<String>, text: impl Into<String>) -> Self {
    Self::new(model, [text.into()])
  }

  /// Sets [`dimensions`](Self::dimensions).
  #[must_use]
  pub fn with_dimensions(mut self, dimensions: usize) -> Self {
    self.dimensions = Some(dimensions);
    self
  }

  /// Validates the request: at least one input, no empty input, and `dimensions` (when set)
  /// above 0. The error names an input by its position, never by its text.
  pub fn validate(&self) -> Result<(), AiError> {
    if self.inputs.is_empty() {
      return Err(AiError::EmptyRequest);
    }
    if let Some(i) = self.inputs.iter().position(String::is_empty) {
      return Err(AiError::InvalidRequest(format!(
        "embedding input {i} is empty"
      )));
    }
    if self.dimensions == Some(0) {
      return Err(AiError::InvalidRequest(
        "embedding dimensions must be at least 1".into(),
      ));
    }
    Ok(())
  }
}

/// The embeddings of a request: one per input, in input order, and the token usage (only
/// [`Usage::input_tokens`] is counted; embedding generates no tokens).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EmbedResponse {
  /// One embedding per input, in input order.
  pub embeddings: Vec<Embedding>,
  /// Token usage.
  pub usage: Usage,
}

impl EmbedResponse {
  /// A response of `embeddings` with no usage.
  pub fn new(embeddings: Vec<Embedding>) -> Self {
    Self {
      embeddings,
      usage: Usage::default(),
    }
  }

  /// Sets [`usage`](Self::usage).
  #[must_use]
  pub fn with_usage(mut self, usage: Usage) -> Self {
    self.usage = usage;
    self
  }

  /// The length of the first vector (`None` for no vectors).
  pub fn dimensions(&self) -> Option<usize> {
    self.embeddings.first().map(Embedding::dimensions)
  }

  /// Checks this answer to `sent` inputs: one vector per input, none empty, all of one length,
  /// which is `expected` when given.
  pub fn check(&self, sent: usize, expected: Option<usize>) -> Result<(), EmbeddingError> {
    if self.embeddings.len() != sent {
      return Err(EmbeddingError::CountMismatch {
        sent,
        received: self.embeddings.len(),
      });
    }
    let mut expected = expected;
    for embedding in &self.embeddings {
      let actual = embedding.dimensions();
      if actual == 0 {
        return Err(AiError::EmptyResponse.into());
      }
      match expected {
        Some(expected) if expected != actual => {
          return Err(EmbeddingError::DimensionMismatch { expected, actual });
        }
        Some(_) => {}
        None => expected = Some(actual),
      }
    }
    Ok(())
  }
}

/// Finite limits of [`Embedder::embed`] (ADR 0013 R4): none is infinite, and the setters keep
/// each at least 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EmbeddingLimits {
  /// Inputs per vendor call; a larger request is split into calls of this size.
  pub max_batch_size: usize,
  /// Inputs per [`Embedder::embed`] call; a larger request fails with
  /// [`AiError::LimitExceeded`] (`embedding_inputs`) before anything is sent.
  pub max_inputs: usize,
}

impl EmbeddingLimits {
  /// Default [`max_batch_size`](Self::max_batch_size): 64.
  pub const DEFAULT_MAX_BATCH_SIZE: usize = 64;
  /// Default [`max_inputs`](Self::max_inputs): 4096.
  pub const DEFAULT_MAX_INPUTS: usize = 4096;

  /// Replaces [`max_batch_size`](Self::max_batch_size) (at least 1).
  #[must_use]
  pub fn with_max_batch_size(mut self, n: usize) -> Self {
    self.max_batch_size = n.max(1);
    self
  }

  /// Replaces [`max_inputs`](Self::max_inputs) (at least 1).
  #[must_use]
  pub fn with_max_inputs(mut self, n: usize) -> Self {
    self.max_inputs = n.max(1);
    self
  }
}

impl Default for EmbeddingLimits {
  fn default() -> Self {
    Self {
      max_batch_size: Self::DEFAULT_MAX_BATCH_SIZE,
      max_inputs: Self::DEFAULT_MAX_INPUTS,
    }
  }
}

/// Errors of an embedding call.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum EmbeddingError {
  /// The request or the vendor call failed (transport, status, limits, JSON, validation).
  #[error(transparent)]
  Ai(#[from] AiError),
  /// A vector's length differs from the expected one: [`EmbedRequest::dimensions`], the first
  /// vector of the response, or the other operand of a similarity.
  #[error("embedding dimension mismatch: expected {expected}, got {actual}")]
  DimensionMismatch {
    /// The expected length.
    expected: usize,
    /// The length found.
    actual: usize,
  },
  /// The vendor returned a different number of vectors than inputs it was sent.
  #[error("embedding count mismatch: sent {sent} inputs, received {received} vectors")]
  CountMismatch {
    /// Inputs sent.
    sent: usize,
    /// Vectors received.
    received: usize,
  },
}

impl EmbeddingError {
  /// Whether this error is safe to retry with backoff (a transient [`AiError`]).
  #[inline]
  pub fn is_transient(&self) -> bool {
    matches!(self, Self::Ai(e) if e.is_transient())
  }
}

/// Injectable embedder service.
pub type EmbedderService = Arc<dyn Embedder>;

/// Vendor-neutral embedding capability.
///
/// An implementation provides [`embed_batch`](Self::embed_batch) (one vendor call of at most
/// [`EmbeddingLimits::max_batch_size`] inputs) and, optionally, its
/// [`limits`](Self::limits); callers use [`embed`](Self::embed), which validates, bounds,
/// splits and checks.
pub trait Embedder: Send + Sync + 'static {
  /// The limits [`embed`](Self::embed) applies (default: [`EmbeddingLimits::default`]).
  fn limits(&self) -> EmbeddingLimits {
    EmbeddingLimits::default()
  }

  /// One vendor call for at most [`EmbeddingLimits::max_batch_size`] inputs, unchecked.
  fn embed_batch(&self, req: EmbedRequest) -> Effect<EmbedResponse, EmbeddingError>;

  /// Embeds every input of `req` ([`embed_in_batches`]): validated, refused over
  /// [`EmbeddingLimits::max_inputs`], split into batches of
  /// [`EmbeddingLimits::max_batch_size`] run in order, each batch checked (one vector per
  /// input, one dimension across the whole response), the results concatenated in input order
  /// and their usage summed.
  fn embed(&self, req: EmbedRequest) -> Effect<EmbedResponse, EmbeddingError> {
    embed_in_batches(self, req)
  }
}

/// The batching behind [`Embedder::embed`], for implementations that override `embed` and
/// still want it.
pub fn embed_in_batches<M>(embedder: &M, req: EmbedRequest) -> Effect<EmbedResponse, EmbeddingError>
where
  M: Embedder + ?Sized,
{
  if let Err(e) = req.validate() {
    return fail(e.into());
  }
  let limits = embedder.limits();
  if req.inputs.len() > limits.max_inputs {
    return fail(
      AiError::LimitExceeded {
        limit: "embedding_inputs",
        max: limits.max_inputs as u64,
      }
      .into(),
    );
  }
  let batch_size = limits.max_batch_size.max(1);
  let EmbedRequest {
    model,
    inputs,
    dimensions,
  } = req;
  let mut calls = Vec::with_capacity(inputs.len().div_ceil(batch_size));
  let mut inputs = inputs.into_iter();
  loop {
    let chunk: Vec<String> = inputs.by_ref().take(batch_size).collect();
    if chunk.is_empty() {
      break;
    }
    let sent = chunk.len();
    let call = embedder.embed_batch(EmbedRequest {
      model: model.clone(),
      inputs: chunk,
      dimensions,
    });
    calls.push((sent, call));
  }
  calls.into_iter().fold(
    succeed(EmbedResponse::new(Vec::new())),
    move |acc, (sent, call)| {
      acc.flat_map(move |acc| {
        call.flat_map(move |part| from_result(merge(acc, part, sent, dimensions)))
      })
    },
  )
}

/// Appends batch `part` (answering `sent` inputs) to `acc`, checked against the request's
/// `dimensions` or, unset, the dimension of the batches before it.
fn merge(
  mut acc: EmbedResponse,
  part: EmbedResponse,
  sent: usize,
  dimensions: Option<usize>,
) -> Result<EmbedResponse, EmbeddingError> {
  part.check(sent, dimensions.or_else(|| acc.dimensions()))?;
  acc.embeddings.extend(part.embeddings);
  acc.usage = Usage::new(
    acc
      .usage
      .input_tokens
      .saturating_add(part.usage.input_tokens),
    acc
      .usage
      .output_tokens
      .saturating_add(part.usage.output_tokens),
  );
  Ok(acc)
}

/// A [`Layer`] providing `embedder` as the [`EmbedderService`].
pub fn provide_embedder<E: Send + 'static>(
  embedder: impl Embedder,
) -> Layer<HCons<EmbedderService, HNil>, E> {
  let embedder: EmbedderService = Arc::new(embedder);
  Layer::succeed(hlist![embedder])
}

/// Embeds `req` with the installed [`Embedder`] capability ([`Embedder::embed`]).
#[inline]
pub fn embed<R, I>(req: EmbedRequest) -> Effect<EmbedResponse, EmbeddingError, R>
where
  R: Selector<EmbedderService, I> + HList,
{
  need::<EmbedderService>().flat_map(move |embedder| embedder.embed(req).narrow())
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::suspend;
  use std::sync::Mutex;

  /// Answers each batch with `[len(text), batch number]` vectors and records batch sizes.
  #[derive(Default)]
  struct Lengths {
    batches: Mutex<Vec<usize>>,
    limits: EmbeddingLimits,
  }

  impl Embedder for Lengths {
    fn limits(&self) -> EmbeddingLimits {
      self.limits
    }

    fn embed_batch(&self, req: EmbedRequest) -> Effect<EmbedResponse, EmbeddingError> {
      let mut batches = self.batches.lock().expect("lock");
      batches.push(req.inputs.len());
      let number = batches.len() as f32;
      let embeddings = req
        .inputs
        .iter()
        .map(|t| Embedding::new(vec![t.len() as f32, number]))
        .collect();
      succeed(EmbedResponse::new(embeddings).with_usage(Usage::new(1, 0)))
    }
  }

  fn run(model: &dyn Embedder, req: EmbedRequest) -> Result<EmbedResponse, EmbeddingError> {
    model
      .embed(req)
      .block_on()
      .into_result()
      .map_err(|c| c.fail_option().expect("typed failure"))
  }

  #[test]
  fn splits_in_order_and_sums_usage() {
    let model = Lengths {
      limits: EmbeddingLimits::default().with_max_batch_size(2),
      ..Lengths::default()
    };
    let resp = run(
      &model,
      EmbedRequest::new("m", ["a", "bb", "ccc", "dddd", "eeeee"]),
    )
    .expect("embed");
    let firsts: Vec<f32> = resp.embeddings.iter().map(|e| e.as_slice()[0]).collect();
    assert_eq!(firsts, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
    assert_eq!(resp.usage, Usage::new(3, 0));
    assert_eq!(*model.batches.lock().expect("lock"), vec![2, 2, 1]);
  }

  #[test]
  fn validation_and_input_cap() {
    let model = Lengths {
      limits: EmbeddingLimits::default().with_max_inputs(2),
      ..Lengths::default()
    };
    assert_eq!(
      run(&model, EmbedRequest::new("m", Vec::<String>::new())),
      Err(AiError::EmptyRequest.into())
    );
    assert!(matches!(
      run(&model, EmbedRequest::new("m", ["a", ""])),
      Err(EmbeddingError::Ai(AiError::InvalidRequest(m))) if m.contains("input 1")
    ));
    assert!(matches!(
      run(&model, EmbedRequest::one("m", "a").with_dimensions(0)),
      Err(EmbeddingError::Ai(AiError::InvalidRequest(_)))
    ));
    assert_eq!(
      run(&model, EmbedRequest::new("m", ["a", "b", "c"])),
      Err(
        AiError::LimitExceeded {
          limit: "embedding_inputs",
          max: 2
        }
        .into()
      )
    );
    assert!(model.batches.lock().expect("lock").is_empty());
  }

  #[test]
  fn response_checks() {
    let two = |a: Vec<f32>, b: Vec<f32>| EmbedResponse::new(vec![a.into(), b.into()]);
    assert_eq!(two(vec![1.0], vec![2.0]).check(2, None), Ok(()));
    assert_eq!(two(vec![1.0], vec![2.0]).check(2, Some(1)), Ok(()));
    assert_eq!(
      two(vec![1.0], vec![2.0]).check(3, None),
      Err(EmbeddingError::CountMismatch {
        sent: 3,
        received: 2
      })
    );
    assert_eq!(
      two(vec![1.0], vec![2.0, 3.0]).check(2, None),
      Err(EmbeddingError::DimensionMismatch {
        expected: 1,
        actual: 2
      })
    );
    assert_eq!(
      two(vec![1.0], vec![2.0]).check(2, Some(4)),
      Err(EmbeddingError::DimensionMismatch {
        expected: 4,
        actual: 1
      })
    );
    assert_eq!(
      two(vec![], vec![]).check(2, None),
      Err(AiError::EmptyResponse.into())
    );
    assert_eq!(EmbedResponse::new(vec![]).dimensions(), None);
  }

  #[test]
  fn batches_must_agree_on_dimensions() {
    /// The second batch answers with 3-dimensional vectors.
    struct Drifting(Mutex<usize>);
    impl Embedder for Drifting {
      fn limits(&self) -> EmbeddingLimits {
        EmbeddingLimits::default().with_max_batch_size(1)
      }
      fn embed_batch(&self, _req: EmbedRequest) -> Effect<EmbedResponse, EmbeddingError> {
        let mut n = self.0.lock().expect("lock");
        *n += 1;
        let dims = if *n == 1 { 2 } else { 3 };
        suspend(move || succeed(EmbedResponse::new(vec![vec![1.0; dims].into()])))
      }
    }
    assert_eq!(
      run(&Drifting(Mutex::new(0)), EmbedRequest::new("m", ["a", "b"])),
      Err(EmbeddingError::DimensionMismatch {
        expected: 2,
        actual: 3
      })
    );
  }

  #[test]
  fn embedding_methods() {
    let e = Embedding::from(vec![3.0, 4.0]);
    assert_eq!(e.norm(), 5.0);
    assert_eq!(e.clone().normalized().as_slice(), &[0.6, 0.8]);
    assert_eq!(
      Embedding::new(vec![0.0, 0.0]).normalized().as_slice(),
      &[0.0, 0.0]
    );
    assert_eq!(e.dot(&e), Ok(25.0));
    assert!((e.cosine(&e).expect("cosine") - 1.0).abs() < 1e-6);
    assert_eq!(e.dimensions(), 2);
    assert_eq!(e.into_vec(), vec![3.0, 4.0]);
  }

  #[test]
  fn request_debug_hides_inputs_and_errors_classify() {
    let req = EmbedRequest::new("m", ["secret text"]).with_dimensions(8);
    let printed = format!("{req:?}");
    assert!(!printed.contains("secret"), "{printed}");
    assert!(printed.contains("<1 inputs redacted>"));
    let transient: EmbeddingError = AiError::Transient {
      status: 503,
      detail: String::new(),
    }
    .into();
    assert!(transient.is_transient());
    assert!(
      !EmbeddingError::CountMismatch {
        sent: 1,
        received: 0
      }
      .is_transient()
    );
    assert_eq!(
      EmbeddingError::DimensionMismatch {
        expected: 1,
        actual: 2
      }
      .to_string(),
      "embedding dimension mismatch: expected 1, got 2"
    );
  }

  #[test]
  fn capability_from_the_environment() {
    let resp = embed::<HCons<EmbedderService, HNil>, _>(EmbedRequest::one("m", "abc"))
      .provide_layer(provide_embedder::<EmbeddingError>(Lengths::default()))
      .block_on()
      .into_result()
      .expect("embed");
    assert_eq!(resp.embeddings[0].as_slice(), &[3.0, 1.0]);
  }
}
