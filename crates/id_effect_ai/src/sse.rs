//! Incremental Server-Sent Events line parser.

/// One parsed SSE field line (`event:` or `data:`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseField {
  /// Field name without colon.
  pub name: String,
  /// Field value (trimmed).
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
}

/// Incremental SSE parser over raw bytes.
#[derive(Debug, Default)]
pub struct SseParser {
  buffer: String,
  current: SseMessage,
}

impl SseParser {
  /// Create an empty parser.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Feed more UTF-8 text; returns completed messages.
  pub fn feed(&mut self, chunk: &str) -> Vec<SseMessage> {
    self.buffer.push_str(chunk);
    let mut out = Vec::new();
    while let Some(pos) = self.buffer.find('\n') {
      let line = self.buffer.drain(..=pos).collect::<String>();
      let line = line.trim_end_matches(['\r', '\n']);
      if line.is_empty() {
        if !self.current.data_lines.is_empty() || self.current.event.is_some() {
          out.push(std::mem::take(&mut self.current));
        }
        continue;
      }
      if let Some((name, value)) = line.split_once(':') {
        let name = name.trim().to_string();
        let value = value.trim_start().to_string();
        match name.as_str() {
          "event" => self.current.event = Some(value),
          "data" => self.current.data_lines.push(value),
          _ => {}
        }
      }
    }
    out
  }

  /// Flush any trailing partial message.
  pub fn finish(self) -> Option<SseMessage> {
    if self.current.data_lines.is_empty() && self.current.event.is_none() {
      None
    } else {
      Some(self.current)
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

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
}
