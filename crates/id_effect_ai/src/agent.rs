//! The bounded agent loop (`feature = "tools"`): model → tools → model until the model answers
//! without asking for a tool, under the budgets and rules of ADR 0015 D9.
//!
//! [`ToolLoop`] sends the request with the definitions of its [`ToolSet`]; when the reply asks
//! for tools, each call goes through [`ToolSet::invoke`] under the loop's [`ToolPolicy`] and its
//! result (or its refusal, as `{"error": ...}`) is fed back as a [`ChatRole::Tool`] message.
//! A refusal never aborts the loop: the model sees it and can recover. Only these end a loop
//! with an error:
//!
//! - the model call fails ([`AiError`] of the adapter);
//! - the reply still asks for tools after [`ToolLoopBudget::max_steps`] model calls
//!   ([`AiError::LimitExceeded`] `tool_loop_steps`; the last calls do not run);
//! - a reply would take the run past [`ToolLoopBudget::max_tool_calls`]
//!   (`tool_loop_tool_calls`; none of that reply's calls run);
//! - the whole run exceeds [`ToolLoopBudget::max_duration`] (`tool_loop_duration_ms`; the
//!   running model or tool call is interrupted).
//!
//! Defaults: 8 steps, 32 tool calls, 60 s, 64 KiB per tool output; each is configurable and
//! none can be made infinite.
//!
//! ```
//! use id_effect::{Effect, Schema, succeed};
//! use id_effect_ai::agent::ToolLoop;
//! use id_effect_ai::testing::ScriptedModel;
//! use id_effect_ai::tools::{Tool, ToolSet};
//! use id_effect_ai::ChatRequest;
//!
//! #[derive(Clone, Debug, Schema)]
//! struct City { name: String }
//! #[derive(Clone, Debug, Schema)]
//! struct Weather { celsius: i64 }
//!
//! let weather = Tool::new("weather", "Current weather in a city", |_c: City| -> Effect<Weather, String> {
//!   succeed(Weather { celsius: 21 })
//! });
//! let model = ScriptedModel::new()
//!   .tool_call("c1", "weather", serde_json::json!({ "name": "Ghent" }))
//!   .reply("It is 21 °C in Ghent.");
//!
//! let outcome = ToolLoop::new(ToolSet::new().with(weather).expect("tool"))
//!   .run_with(model.service(), ChatRequest::user("m", "Weather in Ghent?"))
//!   .block_on()
//!   .into_result()
//!   .expect("loop");
//! assert_eq!(outcome.response.content, "It is 21 °C in Ghent.");
//! assert_eq!((outcome.steps, outcome.tool_calls), (2, 1));
//! // The tool's output went back to the model as a tool message.
//! assert_eq!(model.requests()[1].messages.last().unwrap().content, r#"{"celsius":21}"#);
//! ```

use std::time::Duration;

use id_effect::{Effect, HList, Selector, fail, need, succeed};

use crate::chat::{ChatMessage, ChatRequest, ChatResponse, LanguageModelService, ToolCall, Usage};
use crate::error::AiError;
use crate::tools::{DEFAULT_MAX_TOOL_OUTPUT_BYTES, ToolError, ToolPolicy, ToolSet};

#[cfg(doc)]
use crate::chat::ChatRole;

/// The finite budgets of one [`ToolLoop`] run (ADR 0015 D9). Every setter keeps its value at
/// least 1 (1 ms for the duration): no budget is infinite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolLoopBudget {
  /// Model calls per run.
  pub max_steps: u32,
  /// Tool calls per run, over all steps.
  pub max_tool_calls: u32,
  /// Wall time of the whole run.
  pub max_duration: Duration,
  /// Cap on one tool output's JSON text.
  pub max_tool_output_bytes: usize,
}

impl ToolLoopBudget {
  /// Default [`max_steps`](Self::max_steps): 8.
  pub const DEFAULT_MAX_STEPS: u32 = 8;
  /// Default [`max_tool_calls`](Self::max_tool_calls): 32.
  pub const DEFAULT_MAX_TOOL_CALLS: u32 = 32;
  /// Default [`max_duration`](Self::max_duration): 60 s.
  pub const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(60);
  /// Default [`max_tool_output_bytes`](Self::max_tool_output_bytes): 64 KiB.
  pub const DEFAULT_MAX_TOOL_OUTPUT_BYTES: usize = DEFAULT_MAX_TOOL_OUTPUT_BYTES;

  /// Replaces [`max_steps`](Self::max_steps) (at least 1).
  #[must_use]
  pub fn with_max_steps(mut self, n: u32) -> Self {
    self.max_steps = n.max(1);
    self
  }

  /// Replaces [`max_tool_calls`](Self::max_tool_calls) (at least 1).
  #[must_use]
  pub fn with_max_tool_calls(mut self, n: u32) -> Self {
    self.max_tool_calls = n.max(1);
    self
  }

  /// Replaces [`max_duration`](Self::max_duration) (at least 1 ms).
  #[must_use]
  pub fn with_max_duration(mut self, d: Duration) -> Self {
    self.max_duration = d.max(Duration::from_millis(1));
    self
  }

  /// Replaces [`max_tool_output_bytes`](Self::max_tool_output_bytes) (at least 1).
  #[must_use]
  pub fn with_max_tool_output_bytes(mut self, n: usize) -> Self {
    self.max_tool_output_bytes = n.max(1);
    self
  }
}

impl Default for ToolLoopBudget {
  fn default() -> Self {
    Self {
      max_steps: Self::DEFAULT_MAX_STEPS,
      max_tool_calls: Self::DEFAULT_MAX_TOOL_CALLS,
      max_duration: Self::DEFAULT_MAX_DURATION,
      max_tool_output_bytes: Self::DEFAULT_MAX_TOOL_OUTPUT_BYTES,
    }
  }
}

/// The result of a [`ToolLoop`] run.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ToolLoopOutcome {
  /// The model's final reply (it asked for no tool).
  pub response: ChatResponse,
  /// The conversation sent with the last model call: the request's messages, then each
  /// assistant tool-call turn and its tool results.
  pub messages: Vec<ChatMessage>,
  /// Model calls made.
  pub steps: u32,
  /// Tool calls the model asked for and the loop answered (run or refused).
  pub tool_calls: u32,
  /// Every refused or failed tool call, in order (each was fed back to the model).
  pub refusals: Vec<ToolError>,
  /// Token usage summed over the steps.
  pub usage: Usage,
}

/// A bounded model → tool → model loop over a [`ToolSet`] (see the module docs).
#[derive(Debug, Clone, Default)]
pub struct ToolLoop {
  tools: ToolSet,
  policy: ToolPolicy,
  budget: ToolLoopBudget,
}

/// The state carried from one step to the next.
struct Run {
  tools: ToolSet,
  policy: ToolPolicy,
  budget: ToolLoopBudget,
  model: LanguageModelService,
  req: ChatRequest,
  steps: u32,
  tool_calls: u32,
  refusals: Vec<ToolError>,
  usage: Usage,
}

impl ToolLoop {
  /// A loop over `tools` with the default [`ToolPolicy`] (no writes) and [`ToolLoopBudget`].
  pub fn new(tools: ToolSet) -> Self {
    Self {
      tools,
      ..Self::default()
    }
  }

  /// Replaces the policy (for example `ToolPolicy::default().allow_writes()`).
  #[must_use]
  pub fn with_policy(mut self, policy: ToolPolicy) -> Self {
    self.policy = policy;
    self
  }

  /// Replaces the budget.
  #[must_use]
  pub fn with_budget(mut self, budget: ToolLoopBudget) -> Self {
    self.budget = budget;
    self
  }

  /// The tools.
  pub fn tools(&self) -> &ToolSet {
    &self.tools
  }

  /// The policy.
  pub fn policy(&self) -> &ToolPolicy {
    &self.policy
  }

  /// The budget.
  pub fn budget(&self) -> &ToolLoopBudget {
    &self.budget
  }

  /// Runs the loop against `model`. The request's [`tools`](ChatRequest::tools) are replaced by
  /// this loop's definitions, so the model is offered exactly the tools that can run.
  pub fn run_with(
    &self,
    model: LanguageModelService,
    req: ChatRequest,
  ) -> Effect<ToolLoopOutcome, AiError> {
    let max = self.budget.max_duration;
    let run = Run {
      tools: self.tools.clone(),
      policy: self.policy,
      budget: self.budget,
      model,
      req: req.with_tools(self.tools.definitions()),
      steps: 0,
      tool_calls: 0,
      refusals: Vec::new(),
      usage: Usage::default(),
    };
    step(run).timeout_fail(max, move || AiError::LimitExceeded {
      limit: "tool_loop_duration_ms",
      max: u64::try_from(max.as_millis()).unwrap_or(u64::MAX),
    })
  }

  /// Runs the loop against the [`LanguageModelService`] in the environment.
  pub fn run<R, I>(&self, req: ChatRequest) -> Effect<ToolLoopOutcome, AiError, R>
  where
    R: Selector<LanguageModelService, I> + HList,
  {
    let this = self.clone();
    need::<LanguageModelService>().flat_map(move |model| {
      this
        .run_with(LanguageModelService::clone(&model), req)
        .narrow()
    })
  }
}

/// One model call, then (when it asks for tools) its tool calls and the next step.
fn step(mut run: Run) -> Effect<ToolLoopOutcome, AiError> {
  run.steps += 1;
  run.model.complete(run.req.clone()).flat_map(move |resp| {
    run.usage = Usage::new(
      run
        .usage
        .input_tokens
        .saturating_add(resp.usage.input_tokens),
      run
        .usage
        .output_tokens
        .saturating_add(resp.usage.output_tokens),
    );
    if resp.tool_calls.is_empty() {
      return succeed(ToolLoopOutcome {
        response: resp,
        messages: run.req.messages,
        steps: run.steps,
        tool_calls: run.tool_calls,
        refusals: run.refusals,
        usage: run.usage,
      });
    }
    if run.steps >= run.budget.max_steps {
      return fail(AiError::LimitExceeded {
        limit: "tool_loop_steps",
        max: u64::from(run.budget.max_steps),
      });
    }
    let asked = u32::try_from(resp.tool_calls.len()).unwrap_or(u32::MAX);
    if run.tool_calls.saturating_add(asked) > run.budget.max_tool_calls {
      return fail(AiError::LimitExceeded {
        limit: "tool_loop_tool_calls",
        max: u64::from(run.budget.max_tool_calls),
      });
    }
    run.tool_calls += asked;
    let calls = resp.tool_calls.clone();
    run.req.messages.push(ChatMessage::assistant_tool_calls(
      resp.content,
      resp.tool_calls,
    ));
    answer(run, calls.into_iter().rev().collect()).flat_map(step)
  })
}

/// Runs `pending` (in reverse order: the next call is last) and appends each result.
fn answer(mut run: Run, mut pending: Vec<ToolCall>) -> Effect<Run, AiError> {
  let Some(call) = pending.pop() else {
    return succeed(run);
  };
  run
    .tools
    .invoke(&call, &run.policy, run.budget.max_tool_output_bytes)
    .map(|out| Ok(out.text))
    .catch(|refusal| succeed(Err(refusal)))
    .flat_map(move |result: Result<String, ToolError>| {
      let text = match result {
        Ok(text) => text,
        Err(refusal) => {
          let text = refusal.to_model_text();
          run.refusals.push(refusal);
          text
        }
      };
      run
        .req
        .messages
        .push(ChatMessage::tool_result(call.id, text));
      answer(run, pending)
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn budgets_are_finite_and_at_least_one() {
    let b = ToolLoopBudget::default();
    assert_eq!(
      (b.max_steps, b.max_tool_calls, b.max_tool_output_bytes),
      (8, 32, 64 * 1024)
    );
    assert_eq!(b.max_duration, Duration::from_secs(60));
    let b = b
      .with_max_steps(0)
      .with_max_tool_calls(0)
      .with_max_tool_output_bytes(0)
      .with_max_duration(Duration::ZERO);
    assert_eq!(
      (b.max_steps, b.max_tool_calls, b.max_tool_output_bytes),
      (1, 1, 1)
    );
    assert_eq!(b.max_duration, Duration::from_millis(1));
  }

  #[test]
  fn accessors_return_the_configuration() {
    let lp = ToolLoop::new(ToolSet::new())
      .with_policy(ToolPolicy::default().allow_writes())
      .with_budget(ToolLoopBudget::default().with_max_steps(2));
    assert!(lp.tools().is_empty());
    assert!(lp.policy().writes_allowed());
    assert_eq!(lp.budget().max_steps, 2);
  }
}
