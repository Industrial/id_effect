//! Streaming completions as `id_effect::Stream`, and the bounded server-sent events (SSE)
//! decoder the vendor adapters read their streams with.
//!
//! A vendor stream is decoded incrementally: each pull reads one body chunk from the platform
//! client, feeds it to an [`SseParser`] and turns complete events into [`CompletionChunk`]s. The
//! stream is bounded (ADR 0013 R4): its bytes ([`AiLimits::max_stream_bytes`]), its events
//! ([`AiLimits::max_stream_events`]) and one event's size ([`MAX_SSE_EVENT_BYTES`]) are capped; a
//! stream over a cap fails with [`AiError::LimitExceeded`] and stops reading, which drops (closes)
//! the response body. [`decode_sse_event`] is the decoder's entry point for one event (the
//! `ai_sse_event` fuzz target).
//!
//! [`AiLimits::max_stream_bytes`]: crate::AiLimits::max_stream_bytes
//! [`AiLimits::max_stream_events`]: crate::AiLimits::max_stream_events

use id_effect::{Effect, HList, Selector, Stream, need};
use serde::{Deserialize, Serialize};

use crate::chat::{ChatRequest, LanguageModelService};
use crate::error::AiError;

/// Cap on one server-sent event (its pending line plus accumulated fields): 1 MiB.
pub const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;

/// One streamed token or delta from the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionChunk {
  /// Token text.
  pub delta: String,
  /// True on final chunk.
  pub done: bool,
}

/// Complete with streaming using installed [`LanguageModel`](crate::chat::LanguageModel) capability.
#[inline]
pub fn complete_stream<R, I>(
  req: ChatRequest,
) -> Effect<Stream<CompletionChunk, AiError>, AiError, R>
where
  R: Selector<LanguageModelService, I> + HList,
{
  need::<LanguageModelService>().flat_map(move |model| model.complete_stream(req).narrow())
}

/// Build a mock chunk stream by splitting `text` into `chunk_size` character runs (the last
/// chunk has `done` set).
pub fn mock_chunk_stream(text: &str, chunk_size: usize) -> Stream<CompletionChunk, AiError> {
  let parts: Vec<String> = text
    .chars()
    .collect::<Vec<_>>()
    .chunks(chunk_size.max(1))
    .map(|c| c.iter().collect())
    .collect();
  let n = parts.len();
  let chunks = parts
    .into_iter()
    .enumerate()
    .map(|(i, delta)| CompletionChunk {
      delta,
      done: i + 1 == n,
    })
    .collect();
  Stream::from_vec(chunks)
}

/// One parsed SSE field line (`event:` or `data:`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseField {
  /// Field name without colon.
  pub name: String,
  /// Field value (one leading space removed).
  pub value: String,
}

/// Completed SSE message (one or more field lines terminated by blank line).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseMessage {
  /// Optional `event:` value.
  pub event: Option<String>,
  /// Accumulated `data:` payloads (one per line).
  pub data_lines: Vec<String>,
}

impl SseMessage {
  /// Joined `data:` payload.
  pub fn data(&self) -> String {
    self.data_lines.join("\n")
  }

  fn is_empty(&self) -> bool {
    self.event.is_none() && self.data_lines.is_empty()
  }
}

/// Incremental SSE parser over raw bytes (lines end in `\n` or `\r\n`; comment lines and
/// unknown fields are ignored; invalid UTF-8 is replaced, never a panic).
#[derive(Debug, Default)]
pub struct SseParser {
  buffer: Vec<u8>,
  current: SseMessage,
  current_bytes: usize,
}

impl SseParser {
  /// Create an empty parser.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Feed more UTF-8 text; returns completed messages.
  pub fn feed(&mut self, chunk: &str) -> Vec<SseMessage> {
    self.feed_bytes(chunk.as_bytes())
  }

  /// Feed more bytes (a chunk may end inside a line or a character); returns completed
  /// messages.
  pub fn feed_bytes(&mut self, chunk: &[u8]) -> Vec<SseMessage> {
    self.buffer.extend_from_slice(chunk);
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(offset) = self.buffer[start..].iter().position(|&b| b == b'\n') {
      let end = start + offset;
      let line = Self::line_text(&self.buffer[start..end]);
      start = end + 1;
      if let Some(msg) = self.line(&line) {
        out.push(msg);
      }
    }
    self.buffer.drain(..start);
    out
  }

  /// Bytes held for the event being parsed: the unterminated line plus the fields read so far.
  #[inline]
  pub fn pending_bytes(&self) -> usize {
    self.buffer.len() + self.current_bytes
  }

  /// Flush any trailing partial message (an unterminated last line counts).
  pub fn finish(mut self) -> Option<SseMessage> {
    if !self.buffer.is_empty() {
      let line = Self::line_text(&std::mem::take(&mut self.buffer));
      self.line(&line);
    }
    self.take_pending()
  }

  fn line_text(raw: &[u8]) -> String {
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    String::from_utf8_lossy(raw).into_owned()
  }

  /// Applies one line; a blank line completes the current message.
  fn line(&mut self, line: &str) -> Option<SseMessage> {
    if line.is_empty() {
      return self.take_pending();
    }
    let (name, value) = match line.split_once(':') {
      Some((name, value)) => (name, value.strip_prefix(' ').unwrap_or(value)),
      None => (line, ""),
    };
    match name {
      "event" => self.current.event = Some(value.to_string()),
      "data" => self.current.data_lines.push(value.to_string()),
      _ => return None,
    }
    self.current_bytes += line.len();
    None
  }

  fn take_pending(&mut self) -> Option<SseMessage> {
    self.current_bytes = 0;
    let msg = std::mem::take(&mut self.current);
    (!msg.is_empty()).then_some(msg)
  }
}

/// Decodes the first server-sent event in `raw` (text after its terminating blank line is
/// ignored; an unterminated event counts). `Ok(None)` when `raw` holds no field. Input over
/// [`MAX_SSE_EVENT_BYTES`] is refused with [`AiError::LimitExceeded`]. Never panics, whatever the
/// bytes (the `ai_sse_event` fuzz target).
///
/// ```
/// use id_effect_ai::stream::decode_sse_event;
///
/// let msg = decode_sse_event(b"event: ping\ndata: {}\n\n").unwrap().unwrap();
/// assert_eq!(msg.event.as_deref(), Some("ping"));
/// assert_eq!(msg.data(), "{}");
/// ```
pub fn decode_sse_event(raw: &[u8]) -> Result<Option<SseMessage>, AiError> {
  if raw.len() > MAX_SSE_EVENT_BYTES {
    return Err(AiError::LimitExceeded {
      limit: "sse_event_bytes",
      max: MAX_SSE_EVENT_BYTES as u64,
    });
  }
  let mut parser = SseParser::new();
  match parser.feed_bytes(raw).into_iter().next() {
    Some(first) => Ok(Some(first)),
    None => Ok(parser.finish()),
  }
}

#[cfg(any(feature = "openai", feature = "anthropic", feature = "ollama"))]
#[cfg_attr(
  not(any(feature = "openai", feature = "anthropic")),
  allow(unused_imports)
)]
pub(crate) use vendor::{SseStep, sse_chunk_stream};

#[cfg(any(feature = "openai", feature = "anthropic", feature = "ollama"))]
#[cfg_attr(not(any(feature = "openai", feature = "anthropic")), allow(dead_code))]
mod vendor {
  use std::collections::VecDeque;

  use id_effect::{Effect, Stream, fail, succeed};
  use id_effect_platform::error::HttpError;
  use id_effect_platform::http::HttpBody;

  use super::{CompletionChunk, MAX_SSE_EVENT_BYTES, SseMessage, SseParser};
  use crate::config::AiLimits;
  use crate::error::AiError;

  /// What one vendor event means for the chunk stream.
  #[derive(Debug, PartialEq)]
  pub(crate) enum SseStep {
    /// A non-empty text delta.
    Delta(String),
    /// The vendor's end-of-stream event: emit the final chunk and stop reading.
    Done,
    /// An event without text (pings, metadata).
    Skip,
    /// A vendor error event or an undecodable payload.
    Fail(AiError),
  }

  /// Turns one SSE message into an [`SseStep`] (one per vendor).
  pub(crate) type SseDecoder = fn(&SseMessage) -> SseStep;

  struct SseState {
    body: HttpBody,
    parser: SseParser,
    ready: VecDeque<CompletionChunk>,
    bytes: usize,
    events: usize,
    finished: bool,
    max_bytes: usize,
    max_events: usize,
    decode: SseDecoder,
  }

  /// The chunk stream of a vendor SSE `body` under `limits`, decoded incrementally by `decode`.
  /// Always ends with a `done` chunk (the vendor's end event or the end of the body).
  pub(crate) fn sse_chunk_stream(
    body: HttpBody,
    limits: &AiLimits,
    decode: SseDecoder,
  ) -> Stream<CompletionChunk, AiError> {
    let state = SseState {
      body,
      parser: SseParser::new(),
      ready: VecDeque::new(),
      bytes: 0,
      events: 0,
      finished: false,
      max_bytes: limits.max_stream_bytes,
      max_events: limits.max_stream_events,
      decode,
    };
    Stream::unfold_effect(state, next_chunk)
  }

  fn over(limit: &'static str, max: usize) -> AiError {
    AiError::LimitExceeded {
      limit,
      max: max as u64,
    }
  }

  fn next_chunk(mut state: SseState) -> Effect<Option<(CompletionChunk, SseState)>, AiError> {
    if let Some(chunk) = state.ready.pop_front() {
      return succeed(Some((chunk, state)));
    }
    if state.finished {
      return succeed(None);
    }
    let max_bytes = state.max_bytes;
    state
      .body
      .next_chunk()
      .map_err(move |e| match e {
        HttpError::BodyTooLarge { .. } => over("stream_bytes", max_bytes),
        other => AiError::from(other),
      })
      .flat_map(move |read| match state.absorb(read) {
        Ok(()) => next_chunk(state),
        Err(e) => fail(e),
      })
  }

  impl SseState {
    fn absorb(&mut self, read: Option<Vec<u8>>) -> Result<(), AiError> {
      let Some(bytes) = read else {
        if let Some(msg) = std::mem::take(&mut self.parser).finish() {
          self.event(&msg)?;
        }
        if !self.finished {
          self.done();
        }
        return Ok(());
      };
      self.bytes += bytes.len();
      if self.bytes > self.max_bytes {
        return Err(over("stream_bytes", self.max_bytes));
      }
      for msg in self.parser.feed_bytes(&bytes) {
        self.event(&msg)?;
        if self.finished {
          return Ok(());
        }
      }
      if self.parser.pending_bytes() > MAX_SSE_EVENT_BYTES {
        return Err(over("sse_event_bytes", MAX_SSE_EVENT_BYTES));
      }
      Ok(())
    }

    fn event(&mut self, msg: &SseMessage) -> Result<(), AiError> {
      self.events += 1;
      if self.events > self.max_events {
        return Err(over("stream_events", self.max_events));
      }
      match (self.decode)(msg) {
        SseStep::Delta(delta) => self.ready.push_back(CompletionChunk { delta, done: false }),
        SseStep::Done => self.done(),
        SseStep::Skip => {}
        SseStep::Fail(e) => return Err(e),
      }
      Ok(())
    }

    fn done(&mut self) {
      self.ready.push_back(CompletionChunk {
        delta: String::new(),
        done: true,
      });
      self.finished = true;
    }
  }

  #[cfg(test)]
  mod tests {
    use super::*;

    fn decode(msg: &SseMessage) -> SseStep {
      match msg.data().as_str() {
        "[DONE]" => SseStep::Done,
        "" => SseStep::Skip,
        "boom" => SseStep::Fail(AiError::Upstream("boom".into())),
        text => SseStep::Delta(text.to_string()),
      }
    }

    fn collect(chunks: &[&str], limits: AiLimits) -> Result<Vec<CompletionChunk>, Option<AiError>> {
      let body = HttpBody::from_chunks(chunks.iter().map(|c| c.as_bytes().to_vec()));
      sse_chunk_stream(body, &limits, decode)
        .run_collect()
        .block_on()
        .into_result()
        .map_err(|cause| cause.fail_option())
    }

    fn text(chunks: &[CompletionChunk]) -> String {
      chunks.iter().map(|c| c.delta.as_str()).collect()
    }

    #[test]
    fn deltas_across_chunk_boundaries_then_done_event() {
      let out = collect(
        &[
          "data: he",
          "llo\n\ndata: ",
          "world\n\ndata: [DONE]\n\ndata: late\n\n",
        ],
        AiLimits::default(),
      )
      .expect("stream");
      assert_eq!(text(&out), "helloworld");
      assert!(out.last().unwrap().done);
    }

    #[test]
    fn end_of_body_flushes_and_finishes() {
      let out =
        collect(&["data: a\n\n: comment\n\ndata: tail"], AiLimits::default()).expect("stream");
      assert_eq!(text(&out), "atail");
      assert!(out.last().unwrap().done);
      let out = collect(&[], AiLimits::default()).expect("empty");
      assert_eq!(out.len(), 1);
    }

    #[test]
    fn caps_and_vendor_errors_fail_the_stream() {
      let limits = AiLimits::default().with_max_stream_events(2);
      assert_eq!(
        collect(&["data: a\n\ndata: b\n\ndata: c\n\n"], limits),
        Err(Some(over("stream_events", 2)))
      );
      let limits = AiLimits::default().with_max_stream_bytes(4);
      assert_eq!(
        collect(&["data: abc\n\n"], limits),
        Err(Some(over("stream_bytes", 4)))
      );
      assert_eq!(
        collect(&["data: boom\n\n"], AiLimits::default()),
        Err(Some(AiError::Upstream("boom".into())))
      );
      let huge = format!("data: {}", "x".repeat(MAX_SSE_EVENT_BYTES));
      assert_eq!(
        collect(&[huge.as_str()], AiLimits::default()),
        Err(Some(over("sse_event_bytes", MAX_SSE_EVENT_BYTES)))
      );
      assert_eq!(
        collect(&["data: boom"], AiLimits::default()),
        Err(Some(AiError::Upstream("boom".into())))
      );
    }

    #[test]
    fn body_errors_map_to_ai_errors() {
      assert_eq!(
        over("stream_bytes", 1),
        AiError::LimitExceeded {
          limit: "stream_bytes",
          max: 1
        }
      );
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mock_chunk_stream_emits_parts() {
    let chunks = mock_chunk_stream("abcd", 2)
      .run_collect()
      .block_on()
      .into_result()
      .expect("collect");
    let text: String = chunks.iter().map(|c| c.delta.clone()).collect();
    assert_eq!(text, "abcd");
    assert!(chunks.last().unwrap().done);
  }

  #[test]
  fn completion_chunk_round_trip_json() {
    let chunk = CompletionChunk {
      delta: "tok".into(),
      done: true,
    };
    let json = serde_json::to_string(&chunk).unwrap();
    let back: CompletionChunk = serde_json::from_str(&json).unwrap();
    assert_eq!(back, chunk);
  }

  #[test]
  fn parses_openai_style_chunk() {
    let mut p = SseParser::new();
    let msgs = p.feed("data: {\"x\":1}\n\n");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].data(), "{\"x\":1}");
  }

  #[test]
  fn parses_anthropic_event_and_data() {
    let mut p = SseParser::new();
    let msgs = p.feed("event: content_block_delta\ndata: {\"t\":\"a\"}\n\n");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].event.as_deref(), Some("content_block_delta"));
    assert_eq!(msgs[0].data(), "{\"t\":\"a\"}");
  }

  #[test]
  fn crlf_multiline_data_comments_and_bare_fields() {
    let mut p = SseParser::new();
    let msgs = p.feed("data: a\r\ndata:b\r\n: note\r\nid: 7\r\ndata\r\n\r\n\r\n");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].data_lines, vec!["a", "b", ""]);
    assert_eq!(p.pending_bytes(), 0);
    assert_eq!(p.finish(), None);
  }

  #[test]
  fn split_utf8_and_trailing_partial_message() {
    let mut p = SseParser::new();
    let bytes = "data: é\n".as_bytes();
    assert!(p.feed_bytes(&bytes[..7]).is_empty());
    assert!(p.pending_bytes() > 0);
    assert!(p.feed_bytes(&bytes[7..]).is_empty());
    assert!(p.feed("event: e\ndata: x").is_empty());
    let last = p.finish().expect("trailing");
    assert_eq!(last.data_lines, vec!["é", "x"]);
    assert_eq!(last.event.as_deref(), Some("e"));
  }

  #[test]
  fn decode_sse_event_first_event_partial_empty_and_cap() {
    let msg = decode_sse_event(b"data: 1\n\ndata: 2\n\n")
      .unwrap()
      .unwrap();
    assert_eq!(msg.data(), "1");
    let msg = decode_sse_event(b"data: \xff\xfe").unwrap().unwrap();
    assert_eq!(msg.data(), "\u{fffd}\u{fffd}");
    assert_eq!(decode_sse_event(b"").unwrap(), None);
    assert_eq!(decode_sse_event(b": only a comment\n\n").unwrap(), None);
    let big = vec![b'a'; MAX_SSE_EVENT_BYTES + 1];
    assert!(matches!(
      decode_sse_event(&big),
      Err(AiError::LimitExceeded {
        limit: "sse_event_bytes",
        ..
      })
    ));
    let field = SseField {
      name: "data".into(),
      value: "v".into(),
    };
    assert_eq!(field.clone(), field);
  }
}
