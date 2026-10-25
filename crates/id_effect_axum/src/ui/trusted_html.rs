//! The one door for raw HTML (ADR 0013 R9): everything else in `ui` is escaped by maud.
//!
//! maud escapes every `(expression)` it interpolates, in text and in attribute values. The only
//! way around that is `maud::PreEscaped`, and this module is the only place in
//! `crates/id_effect_axum/src` (and `crates/id_effect_admin/src`) allowed to name it: the
//! `no_preescaped_outside_trusted_module` test greps both trees and fails on any other use. A
//! new entry here is a security review, not a refactor.
//!
//! The allowlist:
//!
//! | Function | Accepts | Why it is safe |
//! |---|---|---|
//! | [`from_static`] | a `&'static str` written in the program's source | no request data can reach a string literal |
//!
//! There is deliberately no function taking a runtime `String`: HTML that depends on input is
//! built with [`html!`](super::html), which escapes it.
//!
//! ```rust
//! use id_effect_axum::ui::{html, trusted_html};
//!
//! const ICON: &str = r#"<svg viewBox="0 0 8 8" aria-hidden="true"><circle cx="4" cy="4" r="3"/></svg>"#;
//! let markup = html! { span.icon { (trusted_html::from_static(ICON)) } };
//! assert!(markup.into_string().contains("<svg"));
//! ```

use maud::{Markup, PreEscaped};

/// Static markup written by the developer (an inline SVG icon, a fixed `<noscript>` notice),
/// emitted without escaping.
///
/// Only string literals and `const`s qualify: the `'static` bound keeps request data out, since a
/// handler's input is never `'static` without leaking it on purpose. Do not `Box::leak` input to
/// get here.
pub fn from_static(html: &'static str) -> Markup {
  PreEscaped(html.to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn from_static_is_emitted_verbatim() {
    let markup = maud::html! { p { (from_static("<b>bold</b>")) } };
    assert_eq!(markup.into_string(), "<p><b>bold</b></p>");
  }
}
