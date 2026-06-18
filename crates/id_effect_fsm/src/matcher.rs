//! Bridge FSM events and states to [`id_effect::HasTag`] / [`id_effect::Matcher`].

use id_effect::{HasTag, Matcher};

/// Event wrapper carrying a string tag for [`Matcher::tag`] routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedEvent<E> {
  /// String discriminant used by matchers.
  pub tag: String,
  /// Underlying FSM event payload.
  pub event: E,
}

impl<E> TaggedEvent<E> {
  /// Wraps `event` with `tag`.
  pub fn new(tag: impl Into<String>, event: E) -> Self {
    Self {
      tag: tag.into(),
      event,
    }
  }
}

impl<E> HasTag for TaggedEvent<E> {
  fn tag(&self) -> &str {
    &self.tag
  }
}

/// State wrapper carrying a string tag for [`Matcher::tag`] routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedState<S> {
  /// String discriminant used by matchers.
  pub tag: String,
  /// Underlying FSM state payload.
  pub state: S,
}

impl<S> TaggedState<S> {
  /// Wraps `state` with `tag`.
  pub fn new(tag: impl Into<String>, state: S) -> Self {
    Self {
      tag: tag.into(),
      state,
    }
  }
}

impl<S> HasTag for TaggedState<S> {
  fn tag(&self) -> &str {
    &self.tag
  }
}

/// Builds a [`Matcher`] that routes on [`TaggedEvent::tag`].
pub fn event_matcher<E: 'static, A: 'static>() -> Matcher<TaggedEvent<E>, A> {
  Matcher::new()
}

/// Builds a [`Matcher`] that routes on [`TaggedState::tag`].
pub fn state_matcher<S: 'static, A: 'static>() -> Matcher<TaggedState<S>, A> {
  Matcher::new()
}

/// Classifies a raw event tag string into a handler result using an exhaustive matcher.
pub fn classify_event<E: 'static, A: 'static>(
  matcher: Matcher<TaggedEvent<E>, A>,
  tag: &str,
  event: E,
) -> A {
  let dispatch = matcher.exhaustive();
  dispatch(TaggedEvent::new(tag, event))
}
