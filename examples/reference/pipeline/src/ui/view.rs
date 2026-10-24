//! The back office's layout and components: one HTML document around every page
//! ([`page`]), the panel every part of the queue sits in ([`Panel`]) and a document's status
//! ([`StatusBadge`]). Everything is maud: interpolated values are escaped, messages come from
//! the request's [`Localizer`].
//!
//! [`respond`] answers the content alone to an htmx fragment request and the whole document to
//! anything else, like `id_effect_axum::ui::respond`, but with the request's language in
//! `<html lang>` and the navigation of the back office.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use id_effect_kit::i18n::{Args, Localizer, t};
use id_effect_kit::ui::htmx::htmx_head;
use id_effect_kit::ui::{Component, DOCTYPE, LayoutContext, Markup, Slots, html};

use crate::domain::DocumentStatus;

/// The path of the review queue.
pub const QUEUE_PATH: &str = "/review";

/// The path the language switch posts to.
pub const LOCALE_PATH: &str = "/review/locale";

/// The id of the queue's table region (htmx swaps it after a publication).
pub const QUEUE_ID: &str = "review-queue";

/// The id of the document panel (htmx swaps it when a document is opened or reviewed).
pub const DETAIL_ID: &str = "review-detail";

/// The htmx event a publication triggers: the queue reloads itself on it.
pub const QUEUE_CHANGED: &str = "queue-changed";

/// The whole document around `content`: `<html lang>` in the request's language, the title,
/// htmx, the masked CSRF token on every htmx request (`hx-headers`), the navigation of `tenant`
/// and the language switch.
pub fn page(
  loc: &Localizer,
  ctx: &LayoutContext,
  tenant: &str,
  title: &str,
  content: Markup,
) -> Markup {
  let admin = crate::admin::tenant_prefix(tenant);
  let site = loc.format("backoffice", &Args::new());
  html! {
    (DOCTYPE)
    html lang=(loc.locale().as_str()) {
      head {
        meta charset="utf-8";
        meta name="viewport" content="width=device-width, initial-scale=1";
        title { (title) " · " (site) }
        (htmx_head())
      }
      body hx-headers=[ctx.hx_headers()] {
        header.backoffice {
          nav {
            a href=(QUEUE_PATH) { (t(loc, "nav-queue", Args::new())) }
            " "
            a href=(admin) { (t(loc, "nav-admin", Args::new())) }
          }
          (language_switch(loc, ctx))
        }
        main { (content) }
      }
    }
  }
}

/// The language switch: a form posting the chosen locale to [`LOCALE_PATH`].
pub fn language_switch(loc: &Localizer, ctx: &LayoutContext) -> Markup {
  let current = loc.locale().as_str();
  html! {
    form.language method="post" action=(LOCALE_PATH) {
      (ctx.csrf_field())
      label {
        (t(loc, "nav-language", Args::new())) " "
        select name="locale" {
          @for code in super::LOCALES {
            option value=(code) selected[code == current] {
              (t(loc, &format!("locale-{code}"), Args::new()))
            }
          }
        }
      }
      " "
      button type="submit" { (t(loc, "nav-switch", Args::new())) }
    }
  }
}

/// `content` with `status`: alone for an htmx fragment request, inside [`page`] otherwise.
pub fn respond(
  loc: &Localizer,
  ctx: &LayoutContext,
  tenant: &str,
  title: &str,
  content: Markup,
  status: StatusCode,
) -> Response {
  let body = if ctx.hx().wants_fragment() {
    content
  } else {
    page(loc, ctx, tenant, title, content)
  };
  (status, body).into_response()
}

/// A titled section, the frame of every part of the queue: its id (what htmx targets), the
/// heading, the children, and an optional `footer` slot.
#[derive(Clone, Debug)]
pub struct Panel {
  /// The element id.
  pub id: &'static str,
  /// The heading.
  pub title: String,
}

impl Component for Panel {
  fn render(&self, slots: &Slots) -> Markup {
    html! {
      section.panel id=(self.id) {
        h2 { (self.title) }
        (slots.children())
        @if slots.has("footer") { footer { (slots.named("footer")) } }
      }
    }
  }
}

/// A document's status, translated (`status-<status>`), with the raw status as `data-status`.
#[derive(Clone, Debug)]
pub struct StatusBadge {
  /// The status column value.
  pub status: String,
  /// Its text in the request's language.
  pub label: String,
}

impl StatusBadge {
  /// The badge of `status` in `loc`'s language (an unknown status shows as it is).
  pub fn new(loc: &Localizer, status: &str) -> Self {
    let label = match DocumentStatus::parse(status) {
      Some(known) => loc.format(&format!("status-{}", known.as_str()), &Args::new()),
      None => status.to_owned(),
    };
    Self {
      status: status.to_owned(),
      label,
    }
  }
}

impl Component for StatusBadge {
  fn render(&self, _slots: &Slots) -> Markup {
    html! { span.status data-status=(self.status) { (self.label) } }
  }
}

#[cfg(test)]
mod tests {
  use id_effect_kit::i18n::Locale;
  use id_effect_kit::ui::HxRequest;
  use id_effect_kit::ui::Render;

  use super::*;

  fn localizer(tag: &str) -> Localizer {
    Localizer::new(super::super::catalog(), Locale::parse(tag).unwrap())
  }

  #[test]
  fn components_escape_and_translate() {
    let de = localizer("de");
    let panel = Panel {
      id: DETAIL_ID,
      title: "<script>".to_owned(),
    }
    .with(Slots::new().child(html! { p { "x" } }).slot("footer", "f"))
    .render()
    .into_string();
    assert!(panel.contains("&lt;script&gt;"), "{panel}");
    assert!(panel.contains("<footer>f</footer>"), "{panel}");
    let badge = StatusBadge::new(&de, "review")
      .alone()
      .render()
      .into_string();
    assert!(badge.contains("in Prüfung"), "{badge}");
    assert!(
      StatusBadge::new(&de, "odd")
        .alone()
        .render()
        .into_string()
        .contains(">odd<")
    );
  }

  #[test]
  fn pages_carry_the_language_and_fragments_do_not() {
    let de = localizer("de");
    let ctx = LayoutContext::default();
    let whole = page(&de, &ctx, "acme", "T", html! { p { "body" } }).into_string();
    assert!(whole.starts_with("<!DOCTYPE html>"));
    assert!(whole.contains("lang=\"de\""));
    assert!(whole.contains("href=\"/admin/acme\""));
    assert!(whole.contains("Prüfwarteschlange"));
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("hx-request", "true".parse().unwrap());
    let htmx = LayoutContext::default().with_hx(HxRequest::from_headers(&headers));
    let res = respond(&de, &htmx, "acme", "T", html! { p { "x" } }, StatusCode::OK);
    assert_eq!(res.status(), StatusCode::OK);
    let res = respond(
      &de,
      &ctx,
      "acme",
      "T",
      html! { p { "x" } },
      StatusCode::CONFLICT,
    );
    assert_eq!(res.status(), StatusCode::CONFLICT);
  }
}
