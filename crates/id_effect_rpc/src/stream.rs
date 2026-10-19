//! NDJSON streaming RPC responses (stream methods at the HTTP edge).

use serde::Serialize;

/// One NDJSON line in a stream RPC response.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RpcStreamChunk<T> {
  /// Operation tag.
  pub tag: String,
  /// Stream item payload.
  pub item: T,
}

/// Serialize one stream chunk as an NDJSON line (including trailing newline).
pub fn encode_stream_chunk<T: Serialize>(tag: &str, item: T) -> Result<Vec<u8>, serde_json::Error> {
  let line = RpcStreamChunk {
    tag: tag.to_owned(),
    item,
  };
  let mut bytes = serde_json::to_vec(&line)?;
  bytes.push(10);
  Ok(bytes)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn encode_stream_chunk_appends_newline() {
    let bytes = encode_stream_chunk("events", serde_json::json!({"n": 1})).expect("enc");
    assert_eq!(bytes.last(), Some(&10));
  }
}
