//! Mail previews (feature `preview`): [`MailPreviews`], an axum router at [`PREVIEWS_PATH`] that
//! lists registered previews and renders each from its fixture data, as HTML, as text and as the
//! raw MIME message a transport would send.
//!
//! Every request renders again, so with [`Templates::from_dir`] in a debug build an edited
//! template shows on the next reload of the page. Mount the router in development only: it
//! serves whatever the fixtures contain to anyone who can reach it.

use std::fmt;
use std::sync::Arc;

use axum::Router;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use minijinja::Value;
use serde::Serialize;

use crate::{Address, MailError, Mailbox, Message, Templates};

/// Where [`MailPreviews::router`] serves: the index at `/_mail/previews`, a preview at
/// `/_mail/previews/{name}`, its views at `/_mail/previews/{name}/{html,text,raw}`.
pub const PREVIEWS_PATH: &str = "/_mail/previews";

/// Registered previews over a [`Templates`] registry: each a name, the mail it renders and the
/// fixture context it renders with.
///
/// ```
/// use id_effect_mail::{MailPreviews, Templates};
///
/// let templates = Templates::new()
///   .with_template("welcome.subject.txt", "Welcome, {{ name }}").unwrap()
///   .with_template("welcome.txt", "Hi {{ name }}").unwrap();
/// let previews = MailPreviews::new(templates)
///   .preview("welcome", "welcome", serde_json::json!({ "name": "Carol" }));
///
/// let message = previews.message("welcome").unwrap().unwrap();
/// assert_eq!(message.subject(), "Welcome, Carol");
///
/// // Merged into the app's router in development:
/// let app: axum::Router = axum::Router::new().merge(previews.router());
/// # drop(app);
/// ```
#[derive(Clone)]
pub struct MailPreviews {
  templates: Templates,
  from: Mailbox,
  to: Mailbox,
  previews: Vec<Preview>,
}

#[derive(Clone)]
struct Preview {
  name: String,
  mail: String,
  context: Value,
}

impl MailPreviews {
  /// No previews yet; messages from `previews@example.com` to `recipient@example.com`.
  pub fn new(templates: Templates) -> Self {
    let address = |a: &str| Mailbox::new(Address::parse(a).expect("a valid address"));
    Self {
      templates,
      from: address("previews@example.com"),
      to: address("recipient@example.com"),
      previews: Vec::new(),
    }
  }

  /// Registers the preview `name`: `mail` rendered with `fixture` (serialized now).
  ///
  /// # Panics
  ///
  /// When `name` is empty, is not made of ASCII letters, digits, `-`, `_` and `.` (it is a path
  /// segment), or is already registered.
  #[must_use]
  #[track_caller]
  pub fn preview<C: Serialize>(
    mut self,
    name: impl Into<String>,
    mail: impl Into<String>,
    fixture: C,
  ) -> Self {
    let name = name.into();
    assert!(
      !name.is_empty()
        && name
          .chars()
          .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
      "MailPreviews::preview: `{name}` is not a path segment of letters, digits, `-`, `_`, `.`"
    );
    assert!(
      self.find(&name).is_none(),
      "MailPreviews::preview: `{name}` is registered twice"
    );
    self.previews.push(Preview {
      name,
      mail: mail.into(),
      context: Value::from_serialize(&fixture),
    });
    self
  }

  /// The sender of every preview message.
  #[must_use]
  pub fn sender(mut self, from: impl Into<Mailbox>) -> Self {
    self.from = from.into();
    self
  }

  /// The recipient of every preview message.
  #[must_use]
  pub fn recipient(mut self, to: impl Into<Mailbox>) -> Self {
    self.to = to.into();
    self
  }

  /// The registered previews' names, in registration order.
  pub fn names(&self) -> impl Iterator<Item = &str> {
    self.previews.iter().map(|p| p.name.as_str())
  }

  fn find(&self, name: &str) -> Option<&Preview> {
    self.previews.iter().find(|p| p.name == name)
  }

  /// Renders the preview `name` into a message, or `None` when there is no such preview. A
  /// template that fails is a [`MailError::InvalidMessage`].
  pub fn message(&self, name: &str) -> Option<Result<Message, MailError>> {
    let preview = self.find(name)?;
    Some(
      self
        .templates
        .message(&preview.mail, &preview.context)
        .map_err(MailError::from)
        .and_then(|builder| builder.from(self.from.clone()).to(self.to.clone()).build()),
    )
  }

  /// The router: `GET` [`PREVIEWS_PATH`] lists the previews, `GET {PREVIEWS_PATH}/{name}` shows
  /// one (subject, sender, recipient, the HTML in a frame, the text), and
  /// `{name}/html`, `{name}/text`, `{name}/raw` serve the HTML body, the text body and the MIME
  /// message. A preview that fails to render answers `500` with the template error.
  pub fn router<S: Clone + Send + Sync + 'static>(self) -> Router<S> {
    let previews = Arc::new(self);
    let index = Arc::clone(&previews);
    let page = Arc::clone(&previews);
    Router::new()
      .route(
        PREVIEWS_PATH,
        get(move || {
          let previews = Arc::clone(&index);
          async move { previews.index() }
        }),
      )
      .route(
        &format!("{PREVIEWS_PATH}/{{name}}"),
        get(move |Path(name): Path<String>| {
          let previews = Arc::clone(&page);
          async move { previews.page(&name) }
        }),
      )
      .route(
        &format!("{PREVIEWS_PATH}/{{name}}/{{view}}"),
        get(move |Path((name, view)): Path<(String, String)>| {
          let previews = Arc::clone(&previews);
          async move { previews.view(&name, &view) }
        }),
      )
  }

  fn index(&self) -> Response {
    let mut body = String::from("<h1>Mail previews</h1>\n<table>\n");
    body.push_str("<tr><th>Preview</th><th>Mail</th><th>Views</th></tr>\n");
    for p in &self.previews {
      let name = escape(&p.name);
      body.push_str(&format!(
        "<tr><td><a href=\"{PREVIEWS_PATH}/{name}\">{name}</a></td><td>{}</td>\
         <td>{}</td></tr>\n",
        escape(&p.mail),
        view_links(&name)
      ));
    }
    body.push_str("</table>\n");
    Html(document("Mail previews", &body)).into_response()
  }

  fn page(&self, name: &str) -> Response {
    let message = match self.message(name) {
      None => return not_found(name),
      Some(Err(e)) => return failed(&e),
      Some(Ok(m)) => m,
    };
    let name = escape(name);
    let mut body = format!(
      "<p><a href=\"{PREVIEWS_PATH}\">All previews</a></p>\n<h1>{}</h1>\n\
       <p>From: {}<br>To: {}<br>{}</p>\n",
      escape(message.subject()),
      escape(&message.from().to_string()),
      escape(&message.to()[0].to_string()),
      view_links(&name)
    );
    if message.html().is_some() {
      body.push_str(&format!(
        "<iframe src=\"{PREVIEWS_PATH}/{name}/html\" style=\"width:100%;height:60vh\"></iframe>\n"
      ));
    }
    if let Some(text) = message.text() {
      body.push_str(&format!("<pre>{}</pre>\n", escape(text)));
    }
    Html(document(message.subject(), &body)).into_response()
  }

  fn view(&self, name: &str, view: &str) -> Response {
    let message = match self.message(name) {
      None => return not_found(name),
      Some(Err(e)) => return failed(&e),
      Some(Ok(m)) => m,
    };
    let missing = |what: &str| {
      (
        StatusCode::NOT_FOUND,
        plain(format!("preview `{name}` has no {what} body")),
      )
        .into_response()
    };
    match view {
      "html" => match message.html() {
        Some(html) => Html(html.to_owned()).into_response(),
        None => missing("HTML"),
      },
      "text" => match message.text() {
        Some(text) => plain(text.to_owned()),
        None => missing("text"),
      },
      "raw" => match message.to_mime() {
        Ok(mime) => plain(mime),
        Err(e) => failed(&e),
      },
      _ => (
        StatusCode::NOT_FOUND,
        plain(format!("no view `{view}`: html, text or raw")),
      )
        .into_response(),
    }
  }
}

impl fmt::Debug for MailPreviews {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MailPreviews")
      .field("templates", &self.templates)
      .field("from", &self.from)
      .field("to", &self.to)
      .field("previews", &self.names().collect::<Vec<_>>())
      .finish()
  }
}

fn view_links(escaped_name: &str) -> String {
  ["html", "text", "raw"]
    .iter()
    .map(|v| format!("<a href=\"{PREVIEWS_PATH}/{escaped_name}/{v}\">{v}</a>"))
    .collect::<Vec<_>>()
    .join(" ")
}

fn document(title: &str, body: &str) -> String {
  format!(
    "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{}</title></head>\n<body>\n{body}\
     </body></html>\n",
    escape(title)
  )
}

fn plain(body: impl Into<axum::body::Body>) -> Response {
  (
    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
    body.into(),
  )
    .into_response()
}

fn not_found(name: &str) -> Response {
  (
    StatusCode::NOT_FOUND,
    plain(format!("no mail preview `{name}`")),
  )
    .into_response()
}

fn failed(error: &MailError) -> Response {
  (StatusCode::INTERNAL_SERVER_ERROR, plain(error.to_string())).into_response()
}

/// `s` with the HTML special characters escaped.
fn escape(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for c in s.chars() {
    match c {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      '"' => out.push_str("&quot;"),
      '\'' => out.push_str("&#x27;"),
      c => out.push(c),
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  fn previews() -> MailPreviews {
    let templates = Templates::new()
      .with_template("note.txt", "Note: {{ body }}")
      .unwrap();
    MailPreviews::new(templates).preview("note", "note", serde_json::json!({ "body": "hi" }))
  }

  #[test]
  fn messages_use_the_preview_mailboxes() {
    let message = previews()
      .recipient(Address::parse("carol@ledger.example").unwrap())
      .message("note")
      .unwrap()
      .unwrap();
    assert_eq!(message.from().address().as_str(), "previews@example.com");
    assert_eq!(message.to()[0].address().as_str(), "carol@ledger.example");
    assert_eq!(message.text(), Some("Note: hi"));
    assert!(previews().message("other").is_none());
  }

  #[test]
  #[should_panic(expected = "registered twice")]
  fn a_name_is_registered_once() {
    let _ = previews().preview("note", "note", ());
  }

  #[test]
  #[should_panic(expected = "not a path segment")]
  fn a_name_is_a_path_segment() {
    let _ = previews().preview("a/b", "note", ());
  }

  #[test]
  fn escape_covers_the_html_specials() {
    assert_eq!(
      escape(r#"<a href="x">'&'</a>"#),
      "&lt;a href=&quot;x&quot;&gt;&#x27;&amp;&#x27;&lt;/a&gt;"
    );
  }
}
