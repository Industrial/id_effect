//! Bridge parser combinators to [`id_effect::Stream`] chunk input.

use crate::parser::{ParseFailure, Parser};
use id_effect::{Chunk, Effect, Stream, fail, succeed};

/// Error when stream collection or parsing fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseStreamError<E, P> {
  /// Upstream stream failure.
  Stream(E),
  /// Parser failure after all chunks were collected.
  Parse(P),
  /// UTF-8 decoding failure for byte streams.
  Utf8(String),
}

impl<E: core::fmt::Debug, P: core::fmt::Debug> core::fmt::Display for ParseStreamError<E, P> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::Stream(err) => write!(f, "stream error: {err:?}"),
      Self::Parse(err) => write!(f, "parse error: {err:?}"),
      Self::Utf8(msg) => write!(f, "utf8 error: {msg}"),
    }
  }
}

impl<E: core::fmt::Debug, P: core::fmt::Debug> std::error::Error for ParseStreamError<E, P> {}

/// Collect [`Stream`] chunks into a flat buffer, then run `parser`.
pub fn parse_stream<I, O, E, P, R>(
  parser: Parser<Vec<I>, O, P>,
  stream: Stream<Chunk<I>, E, R>,
) -> Effect<O, ParseStreamError<E, P>, R>
where
  I: Send + Clone + Sync + 'static,
  O: Send + Sync + 'static,
  E: Send + 'static,
  P: Send + Sync + 'static,
  R: 'static,
{
  stream
    .run_collect()
    .map_error(ParseStreamError::Stream)
    .flat_map(move |chunks| {
      let flat: Vec<I> = chunks.into_iter().flat_map(Chunk::into_vec).collect();
      match parser.parse(flat) {
        Ok((value, _rest)) => succeed(value),
        Err(err) => fail(ParseStreamError::Parse(err)),
      }
    })
}

/// Parse a [`Stream`] of `u8` chunks as UTF-8 text before running a string parser.
pub fn parse_text_stream<O, E, R>(
  parser: Parser<String, O, ParseFailure>,
  stream: Stream<Chunk<u8>, E, R>,
) -> Effect<O, ParseStreamError<E, ParseFailure>, R>
where
  O: Send + Sync + 'static,
  E: Send + 'static,
  R: 'static,
{
  stream
    .run_collect()
    .map_error(ParseStreamError::Stream)
    .flat_map(move |chunks| {
      let bytes: Vec<u8> = chunks.into_iter().flat_map(Chunk::into_vec).collect();
      let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => return fail(ParseStreamError::Utf8(err.to_string())),
      };
      match parser.parse(text) {
        Ok((value, _rest)) => succeed(value),
        Err(err) => fail(ParseStreamError::Parse(err)),
      }
    })
}
