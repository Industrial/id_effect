//! HTTP bodies as [`Stream`]s of [`Bytes`].
//!
//! - [`stream_body`] turns a `Stream<Bytes, E>` into an Axum [`Body`]: a producer fiber pulls the
//!   stream and hands each chunk to the body, one at a time (backpressure: it waits while the
//!   client has not read the previous chunk). Dropping the body (client gone) interrupts the
//!   producer, so the stream's finalizers run. A stream failure (or defect) ends the body with an
//!   error, which aborts the response on the wire.
//! - [`body_stream`] reads a request [`Body`] as a `Stream<Bytes, axum::Error>`, one frame per
//!   pull (trailers are skipped).
//!
//! ```rust
//! use axum::body::Bytes;
//! use axum::response::IntoResponse;
//! use id_effect::Stream;
//! use id_effect_axum::body::stream_body;
//!
//! async fn numbers() -> impl IntoResponse {
//!   let lines = Stream::<u32, std::convert::Infallible>::from_iterable(1..=3)
//!     .map(|n| Bytes::from(format!("{n}\n")));
//!   stream_body(lines)
//! }
//! # let _ = numbers;
//! ```

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use http_body::Frame;
use http_body_util::BodyExt;
use id_effect::Exit;
use id_effect::{Effect, FiberContext, HNil, Stream, from_async, succeed};
use tokio::sync::{Mutex, mpsc};

/// The error a streamed body can end with (what [`axum::Error::new`] accepts).
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

type Item = Result<Bytes, axum::Error>;

/// The `http_body::Body` behind [`stream_body`]; dropping it interrupts the producer fiber.
pub struct EffectBody {
  rx: mpsc::Receiver<Item>,
  producer: Arc<FiberContext>,
}

impl fmt::Debug for EffectBody {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("EffectBody")
      .field("producer", &self.producer.id())
      .finish()
  }
}

impl http_body::Body for EffectBody {
  type Data = Bytes;
  type Error = axum::Error;

  fn poll_frame(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
    match std::task::ready!(self.rx.poll_recv(cx)) {
      Some(Ok(bytes)) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
      Some(Err(e)) => Poll::Ready(Some(Err(e))),
      None => Poll::Ready(None),
    }
  }
}

impl Drop for EffectBody {
  fn drop(&mut self) {
    if !self.producer.is_done() {
      self.producer.request_interrupt();
    }
  }
}

/// Pulls `stream` chunk by chunk into `tx`; stops early when the body is gone.
fn pump<E>(stream: Stream<Bytes, E, HNil>, tx: mpsc::Sender<Item>) -> Effect<(), E, HNil>
where
  E: Send + 'static,
{
  stream.next_chunk().flat_map(move |step| match step {
    None => succeed(()),
    Some((chunk, rest)) => from_async(move || async move {
      for bytes in chunk.into_vec() {
        if tx.send(Ok(bytes)).await.is_err() {
          return Ok(None);
        }
      }
      Ok(Some(tx))
    })
    .flat_map(move |tx| match tx {
      Some(tx) => pump(rest, tx),
      None => succeed(()),
    }),
  })
}

/// An [`EffectBody`] streaming `stream`, produced by a fiber on the global
/// [`EffectExecutor`](crate::EffectExecutor)'s runtime.
pub fn effect_body<E>(stream: Stream<Bytes, E, HNil>) -> EffectBody
where
  E: Into<BoxError> + Send + 'static,
{
  let (tx, rx) = mpsc::channel::<Item>(1);
  let err_tx = tx.clone();
  let producer = pump(stream, tx)
    .exit::<std::convert::Infallible>()
    .flat_map(move |exit| match exit {
      Exit::Success(()) => succeed(()),
      Exit::Failure(cause) => {
        let error = match crate::split_cause(cause) {
          Ok(e) => Some(axum::Error::new(e)),
          Err(rest) => crate::first_defect(&rest)
            .map(|m| axum::Error::new(format!("response stream defect: {m}"))),
        };
        from_async(move || async move {
          if let Some(error) = error {
            let _ = err_tx.send(Err(error)).await;
          }
          Ok(())
        })
      }
    });
  let handle = crate::EffectExecutor::global().runtime().fork(producer);
  EffectBody {
    rx,
    producer: Arc::clone(handle.fiber()),
  }
}

/// An Axum [`Body`] streaming `stream` (see the [module docs](self)).
pub fn stream_body<E>(stream: Stream<Bytes, E, HNil>) -> Body
where
  E: Into<BoxError> + Send + 'static,
{
  Body::new(effect_body(stream))
}

/// A request body as a stream of its data frames.
pub fn body_stream<R>(body: Body) -> Stream<Bytes, axum::Error, R>
where
  R: Send + Sync + 'static,
{
  let body = Arc::new(Mutex::new(body));
  Stream::from_pull(move |_max| {
    let body = Arc::clone(&body);
    from_async(move || async move {
      let mut body = body.lock().await;
      loop {
        match body.frame().await {
          None => return Ok(None),
          Some(Err(e)) => return Err(e),
          Some(Ok(frame)) => {
            if let Ok(data) = frame.into_data() {
              return Ok(Some(vec![data]));
            }
          }
        }
      }
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::Router;
  use axum::http::{Request, StatusCode};
  use axum::routing::{get, post};
  use http_body::Body as _;
  use id_effect::{fail, sleep, sync};
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::time::Duration;
  use tower::ServiceExt;

  async fn collect(body: Body) -> Result<Bytes, axum::Error> {
    body.collect().await.map(|c| c.to_bytes())
  }

  #[tokio::test]
  async fn streams_every_chunk_in_order() {
    let s = Stream::<u32, std::convert::Infallible>::from_iterable(0..200)
      .map(|n| Bytes::from(format!("{n},")));
    let expected: String = (0..200).map(|n| format!("{n},")).collect();
    let got = collect(stream_body(s)).await.unwrap();
    assert_eq!(got, Bytes::from(expected));
  }

  #[tokio::test]
  async fn a_failing_stream_ends_the_body_with_an_error() {
    let s = Stream::<Bytes, String>::from_vec(vec![Bytes::from_static(b"a")])
      .chain(Stream::fail("broken pipe".to_string()));
    let err = collect(stream_body(s)).await.unwrap_err();
    assert!(err.to_string().contains("broken pipe"), "{err}");
  }

  #[tokio::test]
  async fn a_defect_ends_the_body_with_an_error() {
    let s = Stream::<Bytes, String>::from_effect(sync(|| panic!("producer boom")));
    let err = collect(stream_body(s)).await.unwrap_err();
    assert!(err.to_string().contains("producer boom"), "{err}");
  }

  #[tokio::test]
  async fn dropping_the_body_interrupts_the_producer() {
    let finalized = Arc::new(AtomicBool::new(false));
    let f2 = Arc::clone(&finalized);
    let first = Stream::<Bytes, String>::from_vec(vec![Bytes::from_static(b"head")]);
    let forever = Stream::from_effect(
      sleep(Duration::from_secs(3600))
        .map(|()| vec![Bytes::from_static(b"never")])
        .ensuring(sync(move || f2.store(true, Ordering::SeqCst))),
    );
    let mut body = effect_body(first.chain(forever));
    let frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(frame.into_data().unwrap(), Bytes::from_static(b"head"));
    drop(body);
    tokio::time::timeout(Duration::from_secs(10), async {
      while !finalized.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(1)).await;
      }
    })
    .await
    .expect("the producer's finalizer runs once the body is dropped");
  }

  #[tokio::test]
  async fn request_body_round_trips_through_a_stream() {
    let app = Router::new()
      .route(
        "/echo",
        post(|body: Body| async move {
          let upper = body_stream::<HNil>(body).map(|b| Bytes::from(b.to_ascii_uppercase()));
          stream_body(upper)
        }),
      )
      .route(
        "/fail",
        get(|| async { stream_body(Stream::<Bytes, String>::from_effect(fail("x".into()))) }),
      );
    let res = app
      .clone()
      .oneshot(
        Request::builder()
          .method("POST")
          .uri("/echo")
          .body(Body::from("shout"))
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(collect(res.into_body()).await.unwrap(), "SHOUT");
    let res = app
      .oneshot(Request::builder().uri("/fail").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert!(collect(res.into_body()).await.is_err());
  }
}
