//! Response body cap (M2): [`ReqwestHttpClient`] rejects an over-cap `Content-Length` before
//! reading, and stops reading a streamed body as soon as it passes `max_body_bytes`.
//!
//! The servers here are raw one-connection HTTP/1.1 servers on a std thread, so a test controls
//! exactly what goes on the wire and can see how much the client read.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use common::run;
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClient, HttpRequest, HttpResponse, ReqwestHttpClient, StreamingHttpResponse,
};

/// The cap every test uses.
const MAX: usize = 4096;
/// Size of one chunk of a chunked (no `Content-Length`) body.
const CHUNK: usize = 1024;

/// Serves one connection: reads the request head, writes `head`, then hands the stream to
/// `body`. Returns the URL and a handle yielding `body`'s result.
fn serve_once<T: Send + 'static>(
  head: String,
  body: impl FnOnce(&mut TcpStream) -> T + Send + 'static,
) -> (String, JoinHandle<T>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
  let url = format!("http://{}/", listener.local_addr().expect("addr"));
  let handle = thread::spawn(move || {
    let (mut stream, _) = listener.accept().expect("accept");
    stream
      .set_read_timeout(Some(Duration::from_secs(10)))
      .expect("read timeout");
    stream
      .set_write_timeout(Some(Duration::from_secs(10)))
      .expect("write timeout");
    let mut seen = Vec::new();
    let mut buf = [0u8; 1024];
    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
      let n = stream.read(&mut buf).expect("request head");
      assert!(n > 0, "client closed before sending the request");
      seen.extend_from_slice(&buf[..n]);
    }
    stream.write_all(head.as_bytes()).expect("response head");
    body(&mut stream)
  });
  (url, handle)
}

/// Waits for the server thread off the runtime thread, so the client's connection task can
/// still run (and close the socket) meanwhile.
async fn join<T: Send + 'static>(server: JoinHandle<T>) -> T {
  tokio::task::spawn_blocking(move || server.join().expect("server"))
    .await
    .expect("join")
}

/// A `200` head declaring `len` body bytes.
fn content_length_head(len: usize) -> String {
  format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n")
}

/// A `200` head for a chunked body (no `Content-Length`).
fn chunked_head() -> String {
  "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_string()
}

/// One `CHUNK`-byte chunk in chunked transfer encoding.
fn chunk_frame() -> Vec<u8> {
  let mut frame = format!("{CHUNK:x}\r\n").into_bytes();
  frame.extend(std::iter::repeat_n(b'x', CHUNK));
  frame.extend_from_slice(b"\r\n");
  frame
}

/// A GET for `url` capped at [`MAX`], with a timeout well above any test's expected run.
fn capped(url: String) -> HttpRequest {
  let mut req = HttpRequest::get(url);
  req.max_body_bytes = Some(MAX);
  req.timeout = Some(Duration::from_secs(20));
  req
}

async fn execute(req: HttpRequest) -> Result<HttpResponse, HttpError> {
  run(ReqwestHttpClient::default_client().execute(req)).await
}

async fn execute_stream(req: HttpRequest) -> Result<StreamingHttpResponse, HttpError> {
  run(ReqwestHttpClient::default_client().execute_stream(req)).await
}

/// Writes a few body bytes of a far larger declared body, then holds the connection open
/// until the client closes it. A client that waited for the whole body would hang here
/// until its timeout.
fn send_prefix_and_hold(stream: &mut TcpStream) {
  stream.write_all(&[b'x'; 16]).expect("body prefix");
  let mut buf = [0u8; 64];
  while let Ok(n) = stream.read(&mut buf) {
    if n == 0 {
      break;
    }
  }
}

#[tokio::test]
async fn m2_content_length_over_cap_rejected_before_reading() {
  const DECLARED: usize = 1_000_000;

  let (url, server) = serve_once(content_length_head(DECLARED), send_prefix_and_hold);
  let err = execute(capped(url)).await.expect_err("over cap");
  // The reported length is the declared one, not a running total: nothing was read.
  assert!(
    matches!(
      err,
      HttpError::BodyTooLarge {
        len: DECLARED,
        max: MAX
      }
    ),
    "{err:?}"
  );
  join(server).await;

  let (url, server) = serve_once(content_length_head(DECLARED), send_prefix_and_hold);
  let streamed = execute_stream(capped(url)).await.expect("headers ok");
  assert_eq!(streamed.status, 200);
  let err = run(streamed.body.next_chunk()).await.expect_err("over cap");
  assert!(
    matches!(
      err,
      HttpError::BodyTooLarge {
        len: DECLARED,
        max: MAX
      }
    ),
    "{err:?}"
  );
  assert_eq!(run(streamed.body.next_chunk()).await.expect("done"), None);
  drop(streamed);
  join(server).await;
}

#[tokio::test]
async fn m2_streaming_body_over_cap_stops_early() {
  // 64 MiB on offer: far beyond what loopback socket buffers absorb, so the server can only
  // finish if the client keeps reading.
  const OFFERED: usize = 64 * 1024 * 1024;

  let (url, server) = serve_once(chunked_head(), |stream| {
    let frame = chunk_frame();
    let mut sent = 0;
    while sent < OFFERED {
      if stream.write_all(&frame).is_err() {
        return sent;
      }
      sent += CHUNK;
    }
    let _ = stream.write_all(b"0\r\n\r\n");
    sent
  });
  let err = execute(capped(url)).await.expect_err("over cap");
  match err {
    HttpError::BodyTooLarge { len, max } => {
      assert_eq!(max, MAX);
      assert!(len > MAX, "{len}");
    }
    other => panic!("expected BodyTooLarge, got {other:?}"),
  }
  let sent = join(server).await;
  assert!(
    sent < OFFERED,
    "the client read the whole {OFFERED}-byte body before failing"
  );
}

#[tokio::test]
async fn m2_body_exactly_at_cap_is_accepted() {
  let (url, server) = serve_once(content_length_head(MAX), |stream| {
    stream.write_all(&[b'x'; MAX]).expect("body");
  });
  let resp = execute(capped(url)).await.expect("at cap");
  assert_eq!(resp.body, vec![b'x'; MAX]);
  join(server).await;

  let (url, server) = serve_once(chunked_head(), |stream| {
    for _ in 0..MAX / CHUNK {
      stream.write_all(&chunk_frame()).expect("chunk");
    }
    stream.write_all(b"0\r\n\r\n").expect("last chunk");
  });
  let resp = execute(capped(url)).await.expect("at cap");
  assert_eq!(resp.body, vec![b'x'; MAX]);
  join(server).await;

  let (url, server) = serve_once(chunked_head(), |stream| {
    for _ in 0..MAX / CHUNK {
      stream.write_all(&chunk_frame()).expect("chunk");
    }
    stream.write_all(b"0\r\n\r\n").expect("last chunk");
  });
  let streamed = execute_stream(capped(url)).await.expect("headers ok");
  let body = run(streamed.body.collect()).await.expect("at cap");
  assert_eq!(body, vec![b'x'; MAX]);
  join(server).await;
}
