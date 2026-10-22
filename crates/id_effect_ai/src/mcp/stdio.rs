//! The stdio transport: one JSON-RPC message per line (newline-delimited, no embedded
//! newlines), over the process's stdin/stdout or any byte stream pair.
//!
//! Every line is read under the message cap (a longer line is discarded as it arrives and
//! answered with an error, never buffered whole). Each request runs as its own fiber, at most
//! [`McpLimits::max_in_flight`](super::McpLimits) at once; `notifications/cancelled` interrupts
//! the named request's fiber (its finalizers run) and no response is sent for it. At end of
//! input the server finishes the requests in flight, then stops.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use id_effect::{Effect, Executor, FiberContext};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::context::ProgressSink;
use super::error::McpError;
use super::message::{
  DecodeError, INVALID_REQUEST, Message, RequestId, RpcError, SERVER_BUSY,
  decode_message_with_limit,
};
use super::server::McpServer;
use super::{NOTIFICATION_CANCELLED, NOTIFICATION_PROGRESS, STDIO_PRINCIPAL};

/// Lines queued for the writer before progress notifications are dropped.
const WRITE_QUEUE: usize = 256;

/// One line read under the cap.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Line {
  /// End of input.
  Eof,
  /// A line (without its newline).
  Text(Vec<u8>),
  /// A line longer than the cap (discarded).
  TooLong,
}

/// Reads one `\n`-terminated line of at most `max` bytes; a longer line is consumed and
/// discarded as it arrives.
pub(crate) async fn read_line<R: AsyncBufRead + Unpin>(
  reader: &mut R,
  max: usize,
) -> io::Result<Line> {
  let mut buf = Vec::new();
  let mut over = false;
  let mut seen_any = false;
  loop {
    let available = reader.fill_buf().await?;
    if available.is_empty() {
      return Ok(match (seen_any, over) {
        (false, _) => Line::Eof,
        (true, true) => Line::TooLong,
        (true, false) => Line::Text(buf),
      });
    }
    seen_any = true;
    let (chunk, consumed, end) = match available.iter().position(|b| *b == b'\n') {
      Some(i) => (&available[..i], i + 1, true),
      None => (available, available.len(), false),
    };
    if !over && buf.len() + chunk.len() <= max {
      buf.extend_from_slice(chunk);
    } else {
      over = true;
      buf = Vec::new();
    }
    reader.consume(consumed);
    if end {
      return Ok(if over { Line::TooLong } else { Line::Text(buf) });
    }
  }
}

type InFlight = Arc<Mutex<HashMap<RequestId, Arc<FiberContext>>>>;

fn lock(map: &InFlight) -> std::sync::MutexGuard<'_, HashMap<RequestId, Arc<FiberContext>>> {
  map
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl McpServer {
  /// Serves MCP over the process's stdin and stdout until stdin ends (the stdio transport;
  /// the principal is [`STDIO_PRINCIPAL`]: whoever started the process). Write logs to stderr:
  /// stdout carries the protocol.
  pub fn serve_stdio(&self) -> Effect<(), McpError> {
    self.serve_io(tokio::io::stdin(), tokio::io::stdout())
  }

  /// Serves MCP over `reader` and `writer` (newline-delimited JSON-RPC) until `reader` ends,
  /// then finishes the requests in flight.
  pub fn serve_io<R, W>(&self, reader: R, writer: W) -> Effect<(), McpError>
  where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
  {
    let server = self.clone();
    Effect::from_ctx(move |ctx| {
      let exec = Arc::clone(ctx.fiber().executor());
      serve(server, reader, writer, exec)
    })
  }
}

async fn serve<R, W>(
  server: McpServer,
  reader: R,
  writer: W,
  exec: Arc<dyn Executor>,
) -> Result<(), McpError>
where
  R: AsyncRead + Unpin + Send + 'static,
  W: AsyncWrite + Unpin + Send + 'static,
{
  let max = server.limits().max_message_bytes;
  let max_in_flight = server.limits().max_in_flight;
  let (tx, rx) = mpsc::channel::<Option<String>>(WRITE_QUEUE);
  let (written_tx, written) = oneshot::channel();
  exec.spawn(Box::pin(async move {
    let _ = written_tx.send(write_lines(writer, rx).await);
  }));
  let in_flight: InFlight = Arc::default();
  let principal: Arc<str> = Arc::from(STDIO_PRINCIPAL);
  // Every request task holds a clone; `recv` returns `None` once all of them finished.
  let (alive, mut all_done) = mpsc::channel::<()>(1);
  let mut reader = BufReader::new(reader);
  let send = |tx: &mpsc::Sender<Option<String>>, msg: Message| {
    let tx = tx.clone();
    async move { tx.send(Some(msg.to_text())).await.is_ok() }
  };
  let result = loop {
    let bytes = match read_line(&mut reader, max).await {
      Err(e) => break Err(McpError::transport(e)),
      Ok(Line::Eof) => break Ok(()),
      Ok(Line::TooLong) => {
        let err = DecodeError::TooLarge { max }.to_rpc_error();
        send(&tx, Message::error(None, err)).await;
        continue;
      }
      Ok(Line::Text(bytes)) => bytes,
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
      continue;
    }
    match decode_message_with_limit(&bytes, max) {
      Err(e) => {
        send(&tx, Message::error(None, e.to_rpc_error())).await;
      }
      Ok(Message::Request(req)) => {
        let id = req.id.clone();
        let refusal = {
          let map = lock(&in_flight);
          if map.contains_key(&id) {
            Some(RpcError::new(
              INVALID_REQUEST,
              "request id already in flight",
            ))
          } else if map.len() >= max_in_flight {
            Some(RpcError::new(SERVER_BUSY, "too many requests in flight"))
          } else {
            None
          }
        };
        if let Some(e) = refusal {
          send(&tx, Message::error(Some(id), e)).await;
          continue;
        }
        let progress_tx = tx.clone();
        let sink: ProgressSink = Arc::new(move |params| {
          let note = Message::notification(NOTIFICATION_PROGRESS, Some(params));
          // Progress is lossy: a full queue drops it rather than block the tool.
          let _ = progress_tx.try_send(Some(note.to_text()));
        });
        let running = server.start(req, Arc::clone(&principal), Some(sink));
        lock(&in_flight).insert(id.clone(), running.fiber());
        let tx = tx.clone();
        let in_flight = Arc::clone(&in_flight);
        let alive = alive.clone();
        exec.spawn(Box::pin(async move {
          let out = running.await;
          lock(&in_flight).remove(&id);
          if let Some(msg) = out {
            let _ = tx.send(Some(msg.to_text())).await;
          }
          drop(alive);
        }));
      }
      Ok(Message::Notification(n)) if n.method == NOTIFICATION_CANCELLED => {
        let id = n
          .params
          .as_ref()
          .and_then(|p| p.get("requestId"))
          .and_then(RequestId::from_json);
        if let Some(fiber) = id.and_then(|id| lock(&in_flight).get(&id).cloned()) {
          fiber.request_interrupt();
        }
      }
      // Other notifications and responses (the server sends no requests) need nothing.
      Ok(_) => {}
    }
  };
  drop(alive);
  while all_done.recv().await.is_some() {}
  let _ = tx.send(None).await;
  drop(tx);
  let written = match written.await {
    Ok(io) => io.map_err(McpError::transport),
    Err(_) => Err(McpError::transport("the writer stopped")),
  };
  result.and(written)
}

/// Writes queued lines until the `None` sentinel or until every sender is gone.
pub(crate) async fn write_lines<W: AsyncWrite + Unpin>(
  mut writer: W,
  mut rx: mpsc::Receiver<Option<String>>,
) -> io::Result<()> {
  while let Some(Some(line)) = rx.recv().await {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
  }
  // Closing the write side tells the peer this side is done (end of its input).
  writer.shutdown().await
}

#[cfg(test)]
mod tests {
  use super::*;

  async fn lines(input: &[u8], max: usize) -> Vec<Line> {
    let mut reader = BufReader::with_capacity(4, input);
    let mut out = Vec::new();
    loop {
      let line = read_line(&mut reader, max).await.unwrap();
      let eof = line == Line::Eof;
      out.push(line);
      if eof {
        return out;
      }
    }
  }

  #[tokio::test]
  async fn lines_are_read_under_the_cap() {
    assert_eq!(
      lines(b"ab\ncdefgh\n\nxy", 4).await,
      vec![
        Line::Text(b"ab".to_vec()),
        Line::TooLong,
        Line::Text(Vec::new()),
        Line::Text(b"xy".to_vec()),
        Line::Eof,
      ]
    );
    assert_eq!(lines(b"abcdefgh", 4).await, vec![Line::TooLong, Line::Eof]);
  }

  #[tokio::test]
  async fn the_writer_stops_at_the_sentinel() {
    let (tx, rx) = mpsc::channel(4);
    tx.send(Some("a".into())).await.unwrap();
    tx.send(None).await.unwrap();
    tx.send(Some("never".into())).await.unwrap();
    let mut out = Vec::new();
    write_lines(&mut out, rx).await.unwrap();
    assert_eq!(out, b"a\n");
  }
}
