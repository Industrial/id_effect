//! Deterministic embeddings for [`ScriptedModel`] (features `embeddings` + `test-util`, ADR 0015
//! D8): no vendor, no randomness, the same vector for the same text on every run and platform.

use id_effect::{Effect, HCons, HNil, Layer, suspend};

use super::{EmbedRequest, EmbedResponse, Embedder, EmbedderService, Embedding, EmbeddingError};
use crate::chat::Usage;
use crate::http_util::from_result;
use crate::testing::ScriptedModel;

/// FNV-1a, 64 bit: a fixed, platform-independent hash.
fn fnv1a(bytes: &[u8]) -> u64 {
  bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
    (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
  })
}

impl ScriptedModel {
  /// The vector length of a scripted embedding when the request sets no
  /// [`dimensions`](EmbedRequest::dimensions): 64.
  pub const EMBEDDING_DIMENSIONS: usize = 64;

  /// The deterministic embedding of `text` with `dimensions` components (at least 1):
  /// the hashed bag of its lowercase words and character trigrams, normalized to norm 1.
  /// Texts that share words are closer than texts that share none, so similarity code can be
  /// tested without a vendor.
  ///
  /// ```
  /// use id_effect_ai::testing::ScriptedModel;
  ///
  /// let cat = ScriptedModel::embedding_of("the cat sat", 32);
  /// assert_eq!(cat, ScriptedModel::embedding_of("the cat sat", 32));
  /// let near = cat.cosine(&ScriptedModel::embedding_of("a cat sat down", 32)).unwrap();
  /// let far = cat.cosine(&ScriptedModel::embedding_of("quarterly revenue", 32)).unwrap();
  /// assert!(near > far);
  /// ```
  pub fn embedding_of(text: &str, dimensions: usize) -> Embedding {
    let dimensions = dimensions.max(1);
    let mut vector = vec![0.0_f32; dimensions];
    let lower = text.to_lowercase();
    let mut add = |feature: &[u8]| {
      let h = fnv1a(feature);
      let slot = (h % dimensions as u64) as usize;
      vector[slot] += if h >> 63 == 0 { 1.0 } else { -1.0 };
    };
    for word in lower.split_whitespace() {
      add(word.as_bytes());
    }
    let chars: Vec<char> = lower.chars().collect();
    for gram in chars.windows(3) {
      add(gram.iter().collect::<String>().as_bytes());
    }
    if vector.iter().all(|x| *x == 0.0) {
      vector[0] = 1.0;
    }
    Embedding::new(vector).normalized()
  }

  /// This model as an [`EmbedderService`] (sharing the script).
  pub fn embedder_service(&self) -> EmbedderService {
    std::sync::Arc::new(self.clone())
  }

  /// A [`Layer`] providing this model as the [`EmbedderService`].
  pub fn embedder_layer<E: Send + 'static>(&self) -> Layer<HCons<EmbedderService, HNil>, E> {
    super::provide_embedder(self.clone())
  }
}

/// Deterministic embeddings ([`ScriptedModel::embedding_of`]) of
/// [`EmbedRequest::dimensions`] (default [`ScriptedModel::EMBEDDING_DIMENSIONS`]) components;
/// usage counts one input token per whitespace-separated word. The chat script is untouched.
impl Embedder for ScriptedModel {
  fn embed_batch(&self, req: EmbedRequest) -> Effect<EmbedResponse, EmbeddingError> {
    suspend(move || {
      from_result(req.validate().map_err(EmbeddingError::from).map(|()| {
        let dimensions = req.dimensions.unwrap_or(Self::EMBEDDING_DIMENSIONS);
        let words: usize = req
          .inputs
          .iter()
          .map(|t| t.split_whitespace().count())
          .sum();
        let embeddings = req
          .inputs
          .iter()
          .map(|t| Self::embedding_of(t, dimensions))
          .collect();
        EmbedResponse::new(embeddings)
          .with_usage(Usage::new(u32::try_from(words).unwrap_or(u32::MAX), 0))
      }))
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::embeddings::embed;

  #[test]
  fn deterministic_and_dimensioned() {
    let model = ScriptedModel::new();
    let resp = model
      .embed(EmbedRequest::new("m", ["hello world", "hello"]).with_dimensions(8))
      .block_on()
      .into_result()
      .expect("embed");
    assert_eq!(resp.dimensions(), Some(8));
    assert_eq!(resp.usage.input_tokens, 3);
    assert_eq!(
      resp.embeddings[0],
      ScriptedModel::embedding_of("hello world", 8)
    );
    assert!((resp.embeddings[0].norm() - 1.0).abs() < 1e-5);
    assert_eq!(
      ScriptedModel::embedding_of("  ", 0).as_slice(),
      &[1.0],
      "whitespace only still yields a unit vector"
    );
    let resp = embed::<HCons<EmbedderService, HNil>, _>(EmbedRequest::one("m", "x"))
      .provide_layer(model.embedder_layer::<EmbeddingError>())
      .block_on()
      .into_result()
      .expect("layer");
    assert_eq!(resp.dimensions(), Some(ScriptedModel::EMBEDDING_DIMENSIONS));
    let err = model
      .embedder_service()
      .embed_batch(EmbedRequest::new("m", Vec::<String>::new()))
      .block_on()
      .into_result()
      .unwrap_err();
    assert!(err.fail_option().is_some());
    assert_eq!(model.remaining(), 0);
  }
}
