//! Runtime types of the stream kinds, and the frames a running call produces.
//!
//! - [`Duplex`] is what a two-way stream handler yields: a function from the client's item
//!   stream to the server's (both pull-based, so a slow side slows the other).
//! - [`Subscription`] is what a subscription handler yields: a stream of items, each with the
//!   [`Cursor`] a client resumes after.
//! - [`CallFrames`] turns a running stream call into its [`Frame`]s — `item`… then one `exit` —
//!   for any transport that writes frames (NDJSON here; WebSocket, SSE and WebTransport later).
//!   Dropping it drops the call, which interrupts the handler fiber and runs its finalizers.
//! - [`InProcess`] (feature `test-util`) calls every kind of an [`RpcGroup`](crate::RpcGroup)
//!   without a network, through the same dispatch (guards included) as the real transports.
//!
//! ```rust
//! use id_effect::{Never, Stream};
//! use id_effect_rpc::kinds::{Duplex, Subscription, SubscriptionItem};
//! use id_effect_rpc::wire::Cursor;
//!
//! // Echo every item back, doubled.
//! let duplex: Duplex<i64, i64> = Duplex::new(|items| items.map(|n| n * 2));
//! let subscription: Subscription<String> = Subscription::new(Stream::from_vec(vec![
//!   SubscriptionItem::new("a".to_owned()).with_cursor(Cursor::events(1)),
//! ]));
//! # let _ = (duplex, subscription);
//! ```

use std::pin::Pin;
use std::task::{Context, Poll};

use id_effect::{DynEnv, Never, Stream};

use crate::handler::{BoxCall, CallOutcome, OutItem, OutboundReceiver};
use crate::wire::{CallId, Cursor, Frame, FrameKind};

type DuplexFn<In, Out, E> =
  Box<dyn FnOnce(Stream<In, E, DynEnv>) -> Stream<Out, E, DynEnv> + Send + 'static>;

/// A two-way stream: maps the client's items (`In`) to the server's (`Out`).
pub struct Duplex<In, Out, E = Never> {
  run: DuplexFn<In, Out, E>,
}

impl<In, Out, E> Duplex<In, Out, E>
where
  In: Send + 'static,
  Out: Send + 'static,
  E: Send + 'static,
{
  /// From the mapping `f`, which is called once per call with the client's item stream.
  pub fn new(
    f: impl FnOnce(Stream<In, E, DynEnv>) -> Stream<Out, E, DynEnv> + Send + 'static,
  ) -> Self {
    Self { run: Box::new(f) }
  }

  /// The server's stream for the client's `inbound` items.
  pub fn run(self, inbound: Stream<In, E, DynEnv>) -> Stream<Out, E, DynEnv> {
    (self.run)(inbound)
  }
}

impl<In, Out, E> std::fmt::Debug for Duplex<In, Out, E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("Duplex(..)")
  }
}

/// One item of a [`Subscription`].
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionItem<O> {
  /// The position a client resumes after (`None`: not resumable from here).
  pub cursor: Option<Cursor>,
  /// The item.
  pub value: O,
}

impl<O> SubscriptionItem<O> {
  /// An item without a cursor.
  pub fn new(value: O) -> Self {
    Self {
      cursor: None,
      value,
    }
  }

  /// Set the cursor.
  #[must_use]
  pub fn with_cursor(mut self, cursor: Cursor) -> Self {
    self.cursor = Some(cursor);
    self
  }
}

/// A resumable stream of items.
pub struct Subscription<O, E = Never> {
  stream: Stream<SubscriptionItem<O>, E, DynEnv>,
}

impl<O, E> Subscription<O, E>
where
  O: Send + 'static,
  E: Send + 'static,
{
  /// From a stream of items with cursors.
  pub fn new(stream: Stream<SubscriptionItem<O>, E, DynEnv>) -> Self {
    Self { stream }
  }

  /// From a stream of plain items (no cursors: a client cannot resume it).
  pub fn from_stream(stream: Stream<O, E, DynEnv>) -> Self {
    Self::new(stream.map_serial(SubscriptionItem::new))
  }

  /// The items.
  pub fn into_stream(self) -> Stream<SubscriptionItem<O>, E, DynEnv> {
    self.stream
  }
}

impl<O, E> std::fmt::Debug for Subscription<O, E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("Subscription(..)")
  }
}

/// The frames of one running stream call: an `item` frame per [`OutItem`], then one `exit`
/// frame carrying the call's [`WireExit`](crate::wire::WireExit). Dropping it drops the call.
pub struct CallFrames {
  id: CallId,
  run: Option<BoxCall>,
  items: OutboundReceiver,
  exit: Option<CallOutcome>,
  done: bool,
}

impl CallFrames {
  /// Frames of call `id`, whose future is `run` and whose items arrive on `items`.
  pub fn new(id: CallId, run: BoxCall, items: OutboundReceiver) -> Self {
    Self {
      id,
      run: Some(run),
      items,
      exit: None,
      done: false,
    }
  }

  /// The call id on every frame.
  pub fn id(&self) -> CallId {
    self.id
  }

  fn item_frame(&self, item: OutItem) -> Frame {
    let frame = Frame::new(self.id, FrameKind::Item).with_payload(item.value);
    match item.cursor {
      Some(cursor) => frame.with_cursor(cursor),
      None => frame,
    }
  }

  /// Collect every frame (tests and the in-process transport).
  pub async fn collect(self) -> Vec<Frame> {
    futures_util::StreamExt::collect(self).await
  }
}

impl std::fmt::Debug for CallFrames {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CallFrames")
      .field("id", &self.id)
      .field("running", &self.run.is_some())
      .finish_non_exhaustive()
  }
}

/// The `exit` frame of call `id`.
pub fn exit_frame(id: CallId, outcome: &CallOutcome) -> Frame {
  // A tagged union of JSON values always serializes.
  let payload = serde_json::to_value(&outcome.exit).unwrap_or_default();
  Frame::new(id, FrameKind::Exit).with_payload(payload)
}

impl futures_util::Stream for CallFrames {
  type Item = Frame;

  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Frame>> {
    let this = self.get_mut();
    if this.done {
      return Poll::Ready(None);
    }
    if let Some(run) = this.run.as_mut()
      && let Poll::Ready(outcome) = run.as_mut().poll(cx)
    {
      this.exit = Some(outcome);
      this.run = None;
      this.items.close();
    }
    match this.items.poll_recv(cx) {
      Poll::Ready(Some(item)) => Poll::Ready(Some(this.item_frame(item))),
      Poll::Ready(None) => match this.exit.take() {
        Some(outcome) => {
          this.done = true;
          Poll::Ready(Some(exit_frame(this.id, &outcome)))
        }
        // Every sender is gone but the call is still finishing: it wakes us.
        None => Poll::Pending,
      },
      Poll::Pending => Poll::Pending,
    }
  }
}

#[cfg(feature = "test-util")]
pub use in_process::InProcess;

#[cfg(feature = "test-util")]
mod in_process {
  use std::sync::Arc;

  use http::HeaderMap;
  use http::request::Parts;
  use id_effect::DynEnv;
  use serde_json::Value;

  use super::CallFrames;
  use crate::cancel::CallControl;
  use crate::handler::{CallContext, CallOutcome, InboundSender};
  use crate::registry::RpcGroup;
  use crate::server::dispatch::{self, Rejection};
  use crate::wire::{CallId, Cursor, INITIAL_CREDITS};

  /// Calls the procedures of a group in-process (feature `test-util`): lookup, guards, decoding,
  /// error mapping and cancellation as on the network, without one.
  #[derive(Clone)]
  pub struct InProcess {
    group: Arc<RpcGroup>,
    env: DynEnv,
    headers: HeaderMap,
    control: CallControl,
    next_id: Arc<std::sync::atomic::AtomicU64>,
  }

  impl std::fmt::Debug for InProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.debug_struct("InProcess")
        .field("procedures", &self.group.len())
        .finish_non_exhaustive()
    }
  }

  impl InProcess {
    /// Call `group` with `env`.
    pub fn new(group: RpcGroup, env: DynEnv) -> Self {
      Self {
        group: Arc::new(group),
        env,
        headers: HeaderMap::new(),
        control: CallControl::none(),
        next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
      }
    }

    /// Present `headers` to the procedures' guards.
    #[must_use]
    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
      self.headers = headers;
      self
    }

    /// Run every call under `control` (deadline, cancel signal).
    #[must_use]
    pub fn with_control(mut self, control: CallControl) -> Self {
      self.control = control;
      self
    }

    fn head(&self) -> Parts {
      let mut parts = http::Request::new(()).into_parts().0;
      parts.headers = self.headers.clone();
      parts
    }

    fn ctx(&self) -> CallContext {
      let id = self
        .next_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
      CallContext::new(self.env.clone(), format!("in-process-{id}"))
    }

    fn call_id(&self) -> CallId {
      let id = self
        .next_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
      CallId::new(id).unwrap_or(CallId::CONNECTION)
    }

    /// Call a query or mutation.
    pub async fn unary(&self, tag: &str, payload: Value) -> Result<CallOutcome, Rejection> {
      let entry = dispatch::open(&self.group, &self.head(), tag, None).await?;
      Ok(dispatch::start_unary(entry, payload, self.ctx(), self.control.clone())?.await)
    }

    /// Call a server stream.
    pub async fn server_stream(&self, tag: &str, payload: Value) -> Result<CallFrames, Rejection> {
      let entry = dispatch::open(&self.group, &self.head(), tag, None).await?;
      dispatch::start_server_stream(
        entry,
        self.call_id(),
        payload,
        self.ctx(),
        self.control.clone(),
        INITIAL_CREDITS as usize,
      )
    }

    /// Call a client stream with `items`.
    pub async fn client_stream(
      &self,
      tag: &str,
      items: Vec<Value>,
    ) -> Result<CallOutcome, Rejection> {
      let entry = dispatch::open(&self.group, &self.head(), tag, None).await?;
      let (tx, run) = dispatch::start_client_stream(
        entry,
        self.ctx(),
        self.control.clone(),
        INITIAL_CREDITS as usize,
      )?;
      let feed = async move {
        for item in items {
          if !tx.send(item).await {
            break;
          }
        }
      };
      let (outcome, ()) = futures_util::future::join(run, feed).await;
      Ok(outcome)
    }

    /// Open a two-way stream: send items through the sender (drop it to end the input), read
    /// the frames.
    pub async fn bidi(&self, tag: &str) -> Result<(InboundSender, CallFrames), Rejection> {
      let entry = dispatch::open(&self.group, &self.head(), tag, None).await?;
      dispatch::start_bidi(
        entry,
        self.call_id(),
        self.ctx(),
        self.control.clone(),
        INITIAL_CREDITS as usize,
      )
    }

    /// Subscribe, resuming after `after`.
    pub async fn subscribe(
      &self,
      tag: &str,
      payload: Value,
      after: Option<Cursor>,
    ) -> Result<CallFrames, Rejection> {
      let entry = dispatch::open(&self.group, &self.head(), tag, None).await?;
      dispatch::start_subscription(
        entry,
        self.call_id(),
        payload,
        after,
        self.ctx(),
        self.control.clone(),
        INITIAL_CREDITS as usize,
      )
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::handler::outbound;
  use serde_json::json;

  #[tokio::test]
  async fn frames_carry_items_then_the_exit() {
    let (out, rx) = outbound(4);
    let run: BoxCall = Box::pin(async move {
      out
        .send(OutItem {
          cursor: Some(Cursor::sequence(1)),
          value: json!("a"),
        })
        .await;
      out
        .send(OutItem {
          cursor: None,
          value: json!("b"),
        })
        .await;
      CallOutcome::success(serde_json::Value::Null)
    });
    let frames = CallFrames::new(CallId::new(5).unwrap(), run, rx);
    assert_eq!(frames.id().get(), 5);
    assert!(format!("{frames:?}").contains("running"));
    let frames = frames.collect().await;
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].cursor, Some(Cursor::sequence(1)));
    assert_eq!(frames[1].payload, Some(json!("b")));
    assert_eq!(frames[2].kind, FrameKind::Exit);
    assert_eq!(frames[2].payload.as_ref().unwrap()["_tag"], "Success");
  }

  #[tokio::test]
  async fn frames_wait_for_a_call_that_outlives_its_senders() {
    let (out, rx) = outbound(1);
    let run: BoxCall = Box::pin(async move {
      drop(out);
      tokio::time::sleep(std::time::Duration::from_millis(5)).await;
      CallOutcome::success(json!(1))
    });
    let frames = CallFrames::new(CallId::CONNECTION, run, rx).collect().await;
    assert_eq!(frames.len(), 1);
    let mut done = CallFrames::new(
      CallId::CONNECTION,
      Box::pin(async { CallOutcome::success(json!(1)) }),
      outbound(1).1,
    );
    use futures_util::StreamExt as _;
    assert!(done.next().await.is_some());
    assert!(done.next().await.is_none());
    assert!(done.next().await.is_none());
  }

  #[test]
  fn kinds_debug_without_contents() {
    let duplex: Duplex<i64, i64> = Duplex::new(|s| s);
    assert_eq!(format!("{duplex:?}"), "Duplex(..)");
    let sub: Subscription<i64> = Subscription::from_stream(Stream::from_vec(vec![1]));
    assert_eq!(format!("{sub:?}"), "Subscription(..)");
    let item = SubscriptionItem::new(1).with_cursor(Cursor::events(2));
    assert_eq!(item.cursor, Some(Cursor::events(2)));
  }
}
