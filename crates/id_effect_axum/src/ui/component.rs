//! Components: a props struct that renders itself around [`Slots`] of child markup.
//!
//! A component is any type implementing [`Component`]; its fields are the props. The children it
//! wraps arrive as [`Slots`]: an ordered default slot ([`Slots::child`]) and named slots
//! ([`Slots::slot`]). [`Component::with`] pairs the props with their slots as a [`Slotted`] value,
//! which is [`Render`], so a component drops into any [`html!`](super::html) template or into
//! another component's slot. Everything a component interpolates is escaped by maud.
//!
//! ```rust
//! use id_effect_axum::ui::{Component, Markup, Slots, html, maud};
//!
//! struct Card<'a> {
//!   title: &'a str,
//! }
//!
//! impl Component for Card<'_> {
//!   fn render(&self, slots: &Slots) -> Markup {
//!     html! {
//!       section.card {
//!         h2 { (self.title) }
//!         (slots.children())
//!         @if slots.has("footer") { footer { (slots.named("footer")) } }
//!       }
//!     }
//!   }
//! }
//!
//! let card = Card { title: "<Inbox>" }.with(
//!   Slots::new()
//!     .child(html! { p { "first" } })
//!     .child(html! { p { "second" } })
//!     .slot("footer", "3 unread"),
//! );
//! let page = html! { main { (card) } };
//! assert_eq!(
//!   page.into_string(),
//!   "<main><section class=\"card\"><h2>&lt;Inbox&gt;</h2><p>first</p><p>second</p>\
//!    <footer>3 unread</footer></section></main>"
//! );
//! ```

use std::borrow::Cow;

use axum::response::{IntoResponse, Response};
use maud::{Markup, Render, html};

/// Child markup handed to a [`Component`]: the default slot (in insertion order) and named slots
/// (each in insertion order; a name may be filled several times).
#[derive(Clone, Debug, Default)]
pub struct Slots {
  children: Vec<Markup>,
  named: Vec<(Cow<'static, str>, Markup)>,
}

impl Slots {
  /// No children.
  pub fn new() -> Self {
    Self::default()
  }

  /// Append `child` to the default slot (rendered by [`Slots::children`] in this order).
  pub fn child(mut self, child: impl Render) -> Self {
    self.children.push(child.render());
    self
  }

  /// Append `content` to the slot `name` (rendered by [`Slots::named`]).
  pub fn slot(mut self, name: impl Into<Cow<'static, str>>, content: impl Render) -> Self {
    self.named.push((name.into(), content.render()));
    self
  }

  /// The default slot's children, rendered one after another in insertion order.
  pub fn children(&self) -> SlotContent<'_> {
    SlotContent(self.children.iter().collect())
  }

  /// Everything put into slot `name`, in insertion order (renders nothing when it is empty).
  pub fn named(&self, name: &str) -> SlotContent<'_> {
    SlotContent(
      self
        .named
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, m)| m)
        .collect(),
    )
  }

  /// Whether slot `name` has content.
  pub fn has(&self, name: &str) -> bool {
    self.named.iter().any(|(n, _)| n == name)
  }

  /// Whether no slot has content (default or named).
  pub fn is_empty(&self) -> bool {
    self.children.is_empty() && self.named.is_empty()
  }
}

/// The content of one slot, borrowed from [`Slots`]; [`Render`] emits it in order.
#[derive(Clone, Debug)]
pub struct SlotContent<'a>(Vec<&'a Markup>);

impl SlotContent<'_> {
  /// Number of pieces in the slot.
  pub fn len(&self) -> usize {
    self.0.len()
  }

  /// Whether the slot is empty.
  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

impl Render for SlotContent<'_> {
  fn render(&self) -> Markup {
    html! {
      @for piece in &self.0 { (piece) }
    }
  }
}

/// A view: the implementing type's fields are its props, [`Component::render`] draws it around
/// the [`Slots`] it is given.
pub trait Component {
  /// Render the component with `slots` as its children.
  fn render(&self, slots: &Slots) -> Markup;

  /// Pair the props with their children; the result is [`Render`] (and a response).
  fn with(self, slots: Slots) -> Slotted<Self>
  where
    Self: Sized,
  {
    Slotted {
      component: self,
      slots,
    }
  }

  /// The component without children.
  fn alone(self) -> Slotted<Self>
  where
    Self: Sized,
  {
    self.with(Slots::new())
  }
}

/// A [`Component`] with its [`Slots`]: renders as markup and answers as an HTML response.
#[derive(Clone, Debug)]
pub struct Slotted<C> {
  component: C,
  slots: Slots,
}

impl<C> Slotted<C> {
  /// The props.
  pub fn component(&self) -> &C {
    &self.component
  }

  /// The children.
  pub fn slots(&self) -> &Slots {
    &self.slots
  }
}

impl<C: Component> Render for Slotted<C> {
  fn render(&self) -> Markup {
    self.component.render(&self.slots)
  }
}

impl<C: Component> IntoResponse for Slotted<C> {
  fn into_response(self) -> Response {
    Render::render(&self).into_response()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  struct List;

  impl Component for List {
    fn render(&self, slots: &Slots) -> Markup {
      html! { ul { @for _ in 0..1 { (slots.children()) } } }
    }
  }

  struct Item(&'static str);

  impl Component for Item {
    fn render(&self, slots: &Slots) -> Markup {
      html! { li { (self.0) (slots.named("badge")) } }
    }
  }

  #[test]
  fn components_nest_and_keep_order() {
    let list = List.with(
      Slots::new()
        .child(Item("a").alone())
        .child(Item("b").with(Slots::new().slot("badge", "!").slot("badge", "?")))
        .child(Item("<c>").alone()),
    );
    assert_eq!(
      Render::render(&list).into_string(),
      "<ul><li>a</li><li>b!?</li><li>&lt;c&gt;</li></ul>"
    );
    assert_eq!(list.slots().children().len(), 3);
    assert!(!list.slots().is_empty());
    assert!(!list.slots().has("badge"));
    assert!(list.component().render(&Slots::new()).0.contains("<ul>"));
  }

  #[test]
  fn empty_slots_render_nothing() {
    let slots = Slots::new();
    assert!(slots.is_empty());
    assert!(slots.named("x").is_empty());
    assert_eq!(slots.children().render().into_string(), "");
  }

  #[tokio::test]
  async fn slotted_is_an_html_response() {
    let res = Item("x").alone().into_response();
    assert_eq!(res.headers()["content-type"], "text/html; charset=utf-8");
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"<li>x</li>");
  }
}
