//! In-app similarity over a bounded set: dot product, cosine similarity and top-k ranking.
//!
//! Accumulation runs in `f64` and the result is returned as `f32`. Two vectors of different
//! lengths are never compared: that is [`EmbeddingError::DimensionMismatch`] (`expected` is
//! the left operand's length).

use super::{Embedding, EmbeddingError};

fn same_length(a: &[f32], b: &[f32]) -> Result<(), EmbeddingError> {
  if a.len() == b.len() {
    Ok(())
  } else {
    Err(EmbeddingError::DimensionMismatch {
      expected: a.len(),
      actual: b.len(),
    })
  }
}

fn dot64(a: &[f32], b: &[f32]) -> f64 {
  a.iter()
    .zip(b)
    .map(|(x, y)| f64::from(*x) * f64::from(*y))
    .sum()
}

/// The Euclidean norm of `v`.
pub(super) fn norm(v: &[f32]) -> f32 {
  dot64(v, v).sqrt() as f32
}

/// The dot product of `a` and `b` (equal to the cosine similarity for normalized vectors).
pub fn dot_product(a: &[f32], b: &[f32]) -> Result<f32, EmbeddingError> {
  same_length(a, b)?;
  Ok(dot64(a, b) as f32)
}

/// The cosine similarity of `a` and `b`, in `[-1, 1]`; 0 when either vector is zero.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Result<f32, EmbeddingError> {
  same_length(a, b)?;
  let denom = dot64(a, a).sqrt() * dot64(b, b).sqrt();
  if denom == 0.0 {
    return Ok(0.0);
  }
  Ok((dot64(a, b) / denom).clamp(-1.0, 1.0) as f32)
}

/// The `k` candidates most similar to `query` by cosine similarity, as `(index, score)` pairs,
/// best first (ties keep candidate order). Every candidate must have the query's dimensions.
///
/// This is a linear scan: it is meant for a bounded set held by the application (a few
/// thousand vectors), not as a vector database.
pub fn top_k_similar<'a, I>(
  query: &Embedding,
  candidates: I,
  k: usize,
) -> Result<Vec<(usize, f32)>, EmbeddingError>
where
  I: IntoIterator<Item = &'a Embedding>,
{
  let mut scored = candidates
    .into_iter()
    .enumerate()
    .map(|(i, c)| query.cosine(c).map(|score| (i, score)))
    .collect::<Result<Vec<_>, _>>()?;
  scored.sort_by(|a, b| b.1.total_cmp(&a.1));
  scored.truncate(k);
  Ok(scored)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn products_and_mismatches() {
    assert_eq!(dot_product(&[1.0, 2.0], &[3.0, 4.0]), Ok(11.0));
    assert_eq!(
      dot_product(&[1.0], &[1.0, 2.0]),
      Err(EmbeddingError::DimensionMismatch {
        expected: 1,
        actual: 2
      })
    );
    assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]), Ok(0.0));
    assert_eq!(cosine_similarity(&[1.0, 0.0], &[-2.0, 0.0]), Ok(-1.0));
    assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), Ok(0.0));
    assert_eq!(norm(&[3.0, 4.0]), 5.0);
  }

  #[test]
  fn top_k_ranks_best_first() {
    let q = Embedding::new(vec![1.0, 0.0]);
    let docs = vec![
      Embedding::new(vec![0.0, 1.0]),
      Embedding::new(vec![1.0, 0.1]),
      Embedding::new(vec![1.0, 1.0]),
    ];
    let top = top_k_similar(&q, &docs, 2).expect("rank");
    assert_eq!(top.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1, 2]);
    assert!(top[0].1 > top[1].1);
    assert_eq!(top_k_similar(&q, &docs, 10).expect("all").len(), 3);
    let bad = [Embedding::new(vec![1.0])];
    assert!(matches!(
      top_k_similar(&q, &bad, 1),
      Err(EmbeddingError::DimensionMismatch { .. })
    ));
  }
}
