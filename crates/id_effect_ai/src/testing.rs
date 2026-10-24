//! Deterministic test doubles (`feature = "test-util"`, ADR 0015 D8: no live vendor call in CI).
//!
//! [`ScriptedModel`] is a [`LanguageModel`] that answers from a script: each call takes the next
//! [`ScriptedReply`] (a reply, a tool call, a stream of deltas, or a failure) in order and
//! records the request, so a test can assert both what the program asked and what it did with
//! the answer.
//!
//! ```
//! use id_effect::{HCons, HNil};
//! use id_effect_ai::testing::ScriptedModel;
//! use id_effect_ai::{AiError, ChatRequest, LanguageModelService, complete};
//!
//! let model = ScriptedModel::new().reply("first").reply("second");
//! let ask = |text: &str| {
//!   complete::<HCons<LanguageModelService, HNil>, _>(ChatRequest::user("m", text))
//!     .provide_layer(model.layer::<AiError>())
//!     .block_on()
//!     .into_result()
//!     .expect("scripted reply")
//! };
//! assert_eq!(ask("a").content, "first");
//! assert_eq!(ask("b").content, "second");
//! assert_eq!(model.requests().len(), 2);
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use id_effect::{Effect, HCons, HNil, Layer, Stream, fail, succeed, suspend};

use crate::chat::{
  ChatRequest, ChatResponse, LanguageModel, LanguageModelService, ToolCall, provide_language_model,
};
use crate::error::AiError;
use crate::stream::CompletionChunk;

/// One scripted answer.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ScriptedReply {
  /// A buffered reply (text, usage, tool calls). Streamed, it is one delta.
  Reply(ChatResponse),
  /// Streamed deltas, in order. Buffered, they are concatenated.
  Stream(Vec<String>),
  /// A failure of the call.
  Fail(AiError),
}

#[derive(Debug, Default)]
struct Script {
  replies: VecDeque<ScriptedReply>,
  requests: Vec<ChatRequest>,
}

/// A [`LanguageModel`] that replays a script in order and records every request it served.
/// Clones share the script. A call with no reply left fails with [`AiError::Upstream`]; an
/// invalid request fails with its validation error and consumes nothing.
#[derive(Debug, Clone, Default)]
pub struct ScriptedModel {
  script: Arc<Mutex<Script>>,
}

impl ScriptedModel {
  /// An empty script.
  pub fn new() -> Self {
    Self::default()
  }

  fn lock(&self) -> MutexGuard<'_, Script> {
    self.script.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Appends `reply` to the script (also while the model is in use).
  pub fn push(&self, reply: ScriptedReply) {
    self.lock().replies.push_back(reply);
  }

  /// Appends a text reply.
  #[must_use]
  pub fn reply(self, text: impl Into<String>) -> Self {
    self.respond(ChatResponse::text(text))
  }

  /// Appends a full response (usage, stop reason, tool calls).
  #[must_use]
  pub fn respond(self, response: ChatResponse) -> Self {
    self.push(ScriptedReply::Reply(response));
    self
  }

  /// Appends a reply that asks for one tool call (empty text).
  #[must_use]
  pub fn tool_call(
    self,
    id: impl Into<String>,
    name: impl Into<String>,
    arguments: serde_json::Value,
  ) -> Self {
    self.respond(ChatResponse::text("").with_tool_calls(vec![ToolCall::new(id, name, arguments)]))
  }

  /// Appends a streamed reply of `deltas`.
  #[must_use]
  pub fn stream<I, S>(self, deltas: I) -> Self
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    self.push(ScriptedReply::Stream(
      deltas.into_iter().map(Into::into).collect(),
    ));
    self
  }

  /// Appends a failure.
  #[must_use]
  pub fn fail(self, error: AiError) -> Self {
    self.push(ScriptedReply::Fail(error));
    self
  }

  /// The requests served so far, in order.
  pub fn requests(&self) -> Vec<ChatRequest> {
    self.lock().requests.clone()
  }

  /// Replies not yet used.
  pub fn remaining(&self) -> usize {
    self.lock().replies.len()
  }

  /// This model as a [`LanguageModelService`] (sharing the script).
  pub fn service(&self) -> LanguageModelService {
    Arc::new(self.clone())
  }

  /// A [`Layer`] providing this model (sharing the script).
  pub fn layer<E: Send + 'static>(&self) -> Layer<HCons<LanguageModelService, HNil>, E> {
    provide_language_model(self.clone())
  }

  /// Validates `req`, records it and takes the next reply.
  fn next(&self, req: ChatRequest) -> Result<ScriptedReply, AiError> {
    req.validate()?;
    let mut script = self.lock();
    script.requests.push(req);
    let call = script.requests.len();
    script.replies.pop_front().ok_or_else(|| {
      AiError::Upstream(format!(
        "ScriptedModel: no scripted reply left for call {call}"
      ))
    })
  }
}

impl LanguageModel for ScriptedModel {
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError> {
    let this = self.clone();
    suspend(move || match this.next(req) {
      Ok(ScriptedReply::Reply(resp)) => succeed(resp),
      Ok(ScriptedReply::Stream(deltas)) => succeed(ChatResponse::text(deltas.concat())),
      Ok(ScriptedReply::Fail(e)) | Err(e) => fail(e),
    })
  }

  fn complete_stream(&self, req: ChatRequest) -> Effect<Stream<CompletionChunk, AiError>, AiError> {
    let this = self.clone();
    suspend(move || {
      let deltas = match this.next(req) {
        Ok(ScriptedReply::Reply(resp)) => vec![resp.content],
        Ok(ScriptedReply::Stream(deltas)) => deltas,
        Ok(ScriptedReply::Fail(e)) | Err(e) => return fail(e),
      };
      let mut chunks: Vec<CompletionChunk> = deltas
        .into_iter()
        .filter(|d| !d.is_empty())
        .map(|delta| CompletionChunk { delta, done: false })
        .collect();
      chunks.push(CompletionChunk {
        delta: String::new(),
        done: true,
      });
      succeed(Stream::from_vec(chunks))
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn collect(model: &ScriptedModel, text: &str) -> Result<Vec<CompletionChunk>, AiError> {
    model
      .complete_stream(ChatRequest::user("m", text))
      .flat_map(|s| s.run_collect())
      .block_on()
      .into_result()
      .map_err(|c| c.fail_option().expect("typed failure"))
  }

  #[test]
  fn streams_replies_and_deltas() {
    let model = ScriptedModel::new()
      .stream(["a", "", "b"])
      .reply("whole")
      .fail(AiError::Timeout);
    let chunks = collect(&model, "1").expect("stream");
    assert_eq!(chunks.len(), 3);
    assert!(chunks[2].done);
    let chunks = collect(&model, "2").expect("reply");
    assert_eq!(chunks[0].delta, "whole");
    assert_eq!(collect(&model, "3"), Err(AiError::Timeout));
    assert!(matches!(collect(&model, "4"), Err(AiError::Upstream(_))));
  }

  #[test]
  fn invalid_requests_consume_nothing() {
    let model = ScriptedModel::new().reply("kept");
    let err = model
      .complete(ChatRequest::new("m", vec![]))
      .block_on()
      .into_result()
      .unwrap_err();
    assert_eq!(err.fail_option(), Some(AiError::EmptyRequest));
    assert_eq!(model.remaining(), 1);
    assert!(model.requests().is_empty());
  }

  #[test]
  fn buffered_stream_reply_and_tool_call() {
    let model = ScriptedModel::new().stream(["x", "y"]).tool_call(
      "c1",
      "lookup",
      serde_json::json!({"id": 7}),
    );
    let service = model.service();
    let resp = service
      .complete(ChatRequest::user("m", "q"))
      .block_on()
      .into_result()
      .expect("concat");
    assert_eq!(resp.content, "xy");
    let resp = service
      .complete(ChatRequest::user("m", "q"))
      .block_on()
      .into_result()
      .expect("tool call");
    assert_eq!(resp.tool_calls[0].name, "lookup");
    assert_eq!(resp.tool_calls[0].arguments["id"], 7);
  }
}
