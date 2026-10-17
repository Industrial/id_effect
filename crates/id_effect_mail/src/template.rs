//! Mail templates: [`Templates`], a minijinja registry rendering a mail's subject, text and HTML
//! bodies from one serializable context ([`RenderedMail`], [`Templates::message`]).
//!
//! A mail `welcome` is up to three templates: `welcome.html` (the HTML body), `welcome.txt` (the
//! text body) and `welcome.subject.txt` (the subject, trimmed). At least one body must exist.
//! Layouts and partials are ordinary templates under any name (`layouts/base.html`,
//! `partials/button.html`) used with `{% extends %}` and `{% include %}`.
//!
//! - HTML templates (`.html`, `.htm`, `.xml`) escape every interpolated value; text templates do
//!   not.
//! - A variable or attribute the context does not have is an error naming the template and line,
//!   never an empty string (minijinja's strict undefined behavior). Test for an optional value
//!   with `{% if x is defined %}` or give it a default with `{{ x | default("") }}`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use minijinja::{Environment, ErrorKind, Template, UndefinedBehavior, Value, path_loader};
use serde::Serialize;

use crate::{MailError, Message, MessageBuilder};

/// A registry of mail templates: added from strings ([`Templates::with_template`]) or read from a
/// directory ([`Templates::from_dir`]). Cloning shares the registry.
///
/// ```
/// use id_effect_mail::Templates;
///
/// let templates = Templates::new()
///   .with_template("layout.html", "<body>{% block body %}{% endblock %}</body>").unwrap()
///   .with_template("welcome.subject.txt", "Welcome, {{ name }}").unwrap()
///   .with_template(
///     "welcome.html",
///     r#"{% extends "layout.html" %}{% block body %}Hi {{ name }}{% endblock %}"#,
///   )
///   .unwrap()
///   .with_template("welcome.txt", "Hi {{ name }}").unwrap();
///
/// let mail = templates.render("welcome", &serde_json::json!({ "name": "<Carol>" })).unwrap();
/// assert_eq!(mail.subject.as_deref(), Some("Welcome, <Carol>"));
/// assert_eq!(mail.html.as_deref(), Some("<body>Hi &lt;Carol&gt;</body>"));
/// assert_eq!(mail.text.as_deref(), Some("Hi <Carol>"));
///
/// // A missing variable is an error, not an empty string.
/// assert!(templates.render("welcome", &serde_json::json!({})).is_err());
/// ```
#[derive(Clone)]
pub struct Templates {
  inner: Arc<Inner>,
}

#[derive(Clone)]
struct Inner {
  /// The added templates and, for a directory, its loader. In reload mode it is never rendered
  /// from directly (that would cache what the loader read), only cloned.
  env: Environment<'static>,
  dir: Option<PathBuf>,
  reload: bool,
}

impl Default for Templates {
  fn default() -> Self {
    Self::new()
  }
}

impl Templates {
  /// The HTML body template of `mail`: `{mail}.html`.
  pub fn html_name(mail: &str) -> String {
    format!("{mail}.html")
  }

  /// The text body template of `mail`: `{mail}.txt`.
  pub fn text_name(mail: &str) -> String {
    format!("{mail}.txt")
  }

  /// The subject template of `mail`: `{mail}.subject.txt`.
  pub fn subject_name(mail: &str) -> String {
    format!("{mail}.subject.txt")
  }

  /// An empty registry.
  pub fn new() -> Self {
    Self::with_env(environment(), None, false)
  }

  /// Templates read from `dir` (names are paths relative to it, `/`-separated). In a debug build
  /// every render re-reads them from disk, so an edited template shows on the next render (the
  /// preview page's reload); in a release build each is read once, on first use
  /// ([`reload`](Self::reload) overrides either).
  ///
  /// Fails with [`TemplateError::Io`] when `dir` is not a readable directory.
  pub fn from_dir(dir: impl Into<PathBuf>) -> Result<Self, TemplateError> {
    let dir = dir.into();
    std::fs::read_dir(&dir)
      .map_err(|e| TemplateError::Io(format!("template directory {}: {e}", dir.display())))?;
    let mut env = environment();
    env.set_loader(path_loader(dir.clone()));
    Ok(Self::with_env(env, Some(dir), cfg!(debug_assertions)))
  }

  fn with_env(env: Environment<'static>, dir: Option<PathBuf>, reload: bool) -> Self {
    Self {
      inner: Arc::new(Inner { env, dir, reload }),
    }
  }

  /// Re-read templates from the directory on every render (`true`), or once (`false`). No effect
  /// on templates added with [`with_template`](Self::with_template).
  #[must_use]
  pub fn reload(mut self, reload: bool) -> Self {
    Arc::make_mut(&mut self.inner).reload = reload;
    self
  }

  /// Adds the template `name` (it takes precedence over a file of the same name). Fails with
  /// [`TemplateError::Render`] on a syntax error.
  pub fn with_template(
    mut self,
    name: impl Into<String>,
    source: impl Into<String>,
  ) -> Result<Self, TemplateError> {
    let name = name.into();
    Arc::make_mut(&mut self.inner)
      .env
      .add_template_owned(name.clone(), source.into())
      .map_err(|e| render_error(&name, &e))?;
    Ok(self)
  }

  /// The template directory, for [`from_dir`](Self::from_dir).
  pub fn dir(&self) -> Option<&Path> {
    self.inner.dir.as_deref()
  }

  /// Whether every render re-reads the directory.
  pub fn reloads(&self) -> bool {
    self.inner.reload && self.inner.dir.is_some()
  }

  /// Renders `mail`'s subject, text and HTML templates with `context`.
  ///
  /// Fails with [`TemplateError::NotFound`] when neither `{mail}.txt` nor `{mail}.html` exists,
  /// and with [`TemplateError::Render`] on a syntax error, a missing variable, a missing layout
  /// or partial, or a context that does not serialize.
  pub fn render<C: Serialize + ?Sized>(
    &self,
    mail: &str,
    context: &C,
  ) -> Result<RenderedMail, TemplateError> {
    let fresh;
    let env = if self.reloads() {
      fresh = self.inner.env.clone();
      &fresh
    } else {
      &self.inner.env
    };
    let context = Value::from_serialize(context);
    let render = |name: String| -> Result<Option<String>, TemplateError> {
      match lookup(env, &name)? {
        Some(template) => template
          .render(&context)
          .map(Some)
          .map_err(|e| render_error(&name, &e)),
        None => Ok(None),
      }
    };
    let html = render(Self::html_name(mail))?;
    let text = render(Self::text_name(mail))?;
    if html.is_none() && text.is_none() {
      return Err(TemplateError::NotFound(mail.to_owned()));
    }
    let subject = render(Self::subject_name(mail))?.map(|s| s.trim().to_owned());
    Ok(RenderedMail {
      subject,
      text,
      html,
    })
  }

  /// [`render`](Self::render)s `mail` into a [`Message::builder`] with its subject and bodies
  /// set; add the sender and recipients and [`build`](MessageBuilder::build) it.
  pub fn message<C: Serialize + ?Sized>(
    &self,
    mail: &str,
    context: &C,
  ) -> Result<MessageBuilder, TemplateError> {
    Ok(self.render(mail, context)?.apply(Message::builder()))
  }
}

impl fmt::Debug for Templates {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Templates")
      .field("dir", &self.inner.dir)
      .field("reload", &self.reloads())
      .finish_non_exhaustive()
  }
}

/// The environment every registry starts from: strict undefined values and minijinja's default
/// auto-escaping (HTML for `.html`, `.htm` and `.xml` names, none otherwise).
fn environment() -> Environment<'static> {
  let mut env = Environment::new();
  env.set_undefined_behavior(UndefinedBehavior::Strict);
  env
}

/// Template `name`, or `None` when there is none of that name.
fn lookup<'e>(
  env: &'e Environment<'static>,
  name: &str,
) -> Result<Option<Template<'e, 'e>>, TemplateError> {
  match env.get_template(name) {
    Ok(template) => Ok(Some(template)),
    Err(e) if e.kind() == ErrorKind::TemplateNotFound => Ok(None),
    Err(e) => Err(render_error(name, &e)),
  }
}

fn render_error(template: &str, error: &minijinja::Error) -> TemplateError {
  let mut message = error.to_string();
  let mut source = std::error::Error::source(error);
  while let Some(cause) = source {
    message.push_str(": ");
    message.push_str(&cause.to_string());
    source = cause.source();
  }
  TemplateError::Render {
    template: template.to_owned(),
    message,
  }
}

/// A rendered mail: whichever of the subject, text and HTML templates exist.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderedMail {
  /// The rendered `{mail}.subject.txt`, trimmed.
  pub subject: Option<String>,
  /// The rendered `{mail}.txt`.
  pub text: Option<String>,
  /// The rendered `{mail}.html`.
  pub html: Option<String>,
}

impl RenderedMail {
  /// Sets the parts that were rendered on `builder`.
  pub fn apply(self, mut builder: MessageBuilder) -> MessageBuilder {
    if let Some(subject) = self.subject {
      builder = builder.subject(subject);
    }
    if let Some(text) = self.text {
      builder = builder.text(text);
    }
    if let Some(html) = self.html {
      builder = builder.html(html);
    }
    builder
  }
}

/// Why a mail did not render.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TemplateError {
  /// The mail has neither a text nor an HTML template.
  NotFound(String),
  /// A template failed: a syntax error, a missing variable, a missing layout or partial, a
  /// context that does not serialize.
  Render {
    /// The template being loaded or rendered.
    template: String,
    /// minijinja's reason, with the template and line it failed at.
    message: String,
  },
  /// The template directory could not be read.
  Io(String),
}

impl fmt::Display for TemplateError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      TemplateError::NotFound(mail) => {
        write!(f, "mail template `{mail}` has no `.txt` or `.html` body")
      }
      TemplateError::Render { template, message } => {
        write!(f, "mail template `{template}`: {message}")
      }
      TemplateError::Io(m) => write!(f, "mail templates: {m}"),
    }
  }
}

impl std::error::Error for TemplateError {}

impl From<TemplateError> for MailError {
  fn from(e: TemplateError) -> Self {
    MailError::InvalidMessage(e.to_string())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn welcome() -> Templates {
    Templates::new()
      .with_template("welcome.txt", "Hi {{ name }}")
      .unwrap()
  }

  #[test]
  fn a_mail_without_a_body_is_not_found() {
    let templates = welcome()
      .with_template("bare.subject.txt", "Only a subject")
      .unwrap();
    assert_eq!(
      templates.render("bare", &()),
      Err(TemplateError::NotFound("bare".into()))
    );
  }

  #[test]
  fn a_syntax_error_names_the_template() {
    let Err(TemplateError::Render { template, .. }) =
      Templates::new().with_template("broken.html", "{% if %}")
    else {
      panic!("a syntax error is refused")
    };
    assert_eq!(template, "broken.html");
  }

  #[test]
  fn missing_variables_and_attributes_are_errors() {
    let templates = welcome()
      .with_template("deep.txt", "{{ user.missing }}")
      .unwrap();
    let Err(TemplateError::Render { template, message }) =
      templates.render("welcome", &serde_json::json!({}))
    else {
      panic!("a missing variable fails")
    };
    assert_eq!(template, "welcome.txt");
    assert!(message.contains("undefined"), "{message}");
    assert!(
      templates
        .render("deep", &serde_json::json!({ "user": {} }))
        .is_err()
    );
  }

  #[test]
  fn defined_tests_and_defaults_handle_optional_values() {
    let templates = Templates::new()
      .with_template(
        "opt.txt",
        "{% if note is defined %}{{ note }}{% else %}-{% endif %}/{{ x | default('d') }}",
      )
      .unwrap();
    let mail = templates.render("opt", &serde_json::json!({})).unwrap();
    assert_eq!(mail.text.as_deref(), Some("-/d"));
  }

  #[test]
  fn rendered_parts_fill_the_builder_and_errors_become_mail_errors() {
    let message = welcome()
      .message("welcome", &serde_json::json!({ "name": "Carol" }))
      .unwrap()
      .from(crate::Address::parse("a@ledger.example").unwrap())
      .to(crate::Address::parse("b@ledger.example").unwrap())
      .build()
      .unwrap();
    assert_eq!(message.text(), Some("Hi Carol"));
    assert_eq!(message.html(), None);
    let error: MailError = TemplateError::NotFound("x".into()).into();
    assert!(matches!(error, MailError::InvalidMessage(m) if m.contains("`x`")));
  }

  #[test]
  fn a_missing_directory_is_an_io_error() {
    assert!(matches!(
      Templates::from_dir("/nonexistent/id-effect-mail-templates"),
      Err(TemplateError::Io(_))
    ));
  }
}
