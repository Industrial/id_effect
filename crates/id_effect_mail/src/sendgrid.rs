//! [`SendGridMailer`]: a [`Mailer`] posting to the SendGrid v3 Mail Send API
//! (`POST /v3/mail/send`, bearer API key, JSON body) through the
//! [`HttpClientService`] capability (release spec 0.10.0 D9).
//!
//! Because it runs on the installed HTTP client, it inherits that client's defaults: the 0.10
//! connect and total timeouts, and its retry policy. That policy retries idempotent methods only,
//! and this mailer does not opt POST in: the Mail Send API takes no idempotency key, so a retry
//! after a timeout or a dropped connection can deliver the message twice. A `429` or `5xx` reply
//! is [`MailError::Temporary`] and a lost connection [`MailError::Transport`]
//! ([`MailError::is_transient`]); retry those at a level that can accept a duplicate, such as a
//! job. (Listing `Post` in the HTTP layer's `RetryPolicy::with_methods` opts every POST of that
//! client in, this one included.)
//!
//! Inline parts are sent as attachments with `disposition: inline` and their `content_id`, so
//! the HTML references them as `cid:<content-id>` as over SMTP. The API key never appears in
//! `Debug` output.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use id_effect::{Effect, Layer, fail, need, succeed};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{HttpClientEnv, HttpClientService, HttpRequest, HttpResponse};
use serde_json::{Map, Value, json};

use crate::{MailError, Mailbox, Mailer, MailerEnv, Message, Sent, mailer_env};

/// The most bytes of an unparseable error body kept in a [`MailError`].
const MAX_REASON_BYTES: usize = 512;

/// Settings for [`SendGridMailer`].
#[derive(Clone)]
pub struct SendGridConfig {
  api_key: String,
  base_url: String,
  sandbox: bool,
  timeout: Option<Duration>,
}

impl SendGridConfig {
  /// The production API origin.
  pub const DEFAULT_BASE_URL: &'static str = "https://api.sendgrid.com";

  /// Sends with `api_key` (kept out of `Debug`) to [`Self::DEFAULT_BASE_URL`].
  pub fn new(api_key: impl Into<String>) -> Self {
    Self {
      api_key: api_key.into(),
      base_url: Self::DEFAULT_BASE_URL.to_owned(),
      sandbox: false,
      timeout: None,
    }
  }

  /// Replaces the API origin (a regional endpoint, or a mock server in tests).
  pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
    self.base_url = base_url.into();
    self
  }

  /// With `true`, SendGrid validates each request and sends nothing (`mail_settings.sandbox_mode`).
  pub fn sandbox(mut self, sandbox: bool) -> Self {
    self.sandbox = sandbox;
    self
  }

  /// A total timeout per send, replacing the HTTP client's default.
  pub fn timeout(mut self, timeout: Duration) -> Self {
    self.timeout = Some(timeout);
    self
  }

  /// The URL messages are posted to.
  pub fn send_url(&self) -> String {
    format!("{}/v3/mail/send", self.base_url.trim_end_matches('/'))
  }

  /// Whether sandbox mode is on.
  pub fn is_sandbox(&self) -> bool {
    self.sandbox
  }
}

impl fmt::Debug for SendGridConfig {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SendGridConfig")
      .field("api_key", &"<redacted>")
      .field("base_url", &self.base_url)
      .field("sandbox", &self.sandbox)
      .field("timeout", &self.timeout)
      .finish()
  }
}

/// A [`Mailer`] over the SendGrid v3 API. Cloning shares the configuration and HTTP client.
#[derive(Clone)]
pub struct SendGridMailer {
  config: Arc<SendGridConfig>,
  client: HttpClientService,
}

impl SendGridMailer {
  /// A mailer for `config` sending through `client`.
  pub fn new(config: SendGridConfig, client: HttpClientService) -> Self {
    Self {
      config: Arc::new(config),
      client,
    }
  }

  /// A [`Layer`] providing a [`SendGridMailer`] for `config` as the
  /// [`MailerService`](crate::MailerService), over the [`HttpClientService`] it needs (feed it
  /// `ReqwestHttpClientProvider::layer()` with `provide_to`).
  pub fn layer<E: Send + 'static>(config: SendGridConfig) -> Layer<MailerEnv, E, HttpClientEnv> {
    Layer::effect(move || {
      let config = config.clone();
      need::<HttpClientService>()
        .map(move |client| mailer_env(SendGridMailer::new(config, (*client).clone())))
    })
  }

  /// The configuration.
  pub fn config(&self) -> &SendGridConfig {
    &self.config
  }

  fn request(&self, message: &Message) -> HttpRequest {
    let mut req = HttpRequest::post(self.config.send_url(), request_body(message, &self.config));
    req.headers = vec![
      (
        "Authorization".to_owned(),
        format!("Bearer {}", self.config.api_key),
      ),
      ("Content-Type".to_owned(), "application/json".to_owned()),
    ];
    req.timeout = self.config.timeout;
    req
  }
}

impl fmt::Debug for SendGridMailer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SendGridMailer")
      .field("config", &self.config)
      .finish_non_exhaustive()
  }
}

impl Mailer for SendGridMailer {
  fn send(&self, message: Message) -> Effect<Sent, MailError> {
    self
      .client
      .execute(self.request(&message))
      .map_err(http_error)
      .flat_map(|resp| match sent(&resp) {
        Ok(sent) => succeed(sent),
        Err(e) => fail(e),
      })
  }
}

fn mailbox(m: &Mailbox) -> Value {
  let mut out = Map::new();
  out.insert("email".into(), Value::from(m.address().as_str()));
  if let Some(name) = m.name() {
    out.insert("name".into(), Value::from(name));
  }
  Value::Object(out)
}

fn mailboxes(list: &[Mailbox]) -> Value {
  Value::Array(list.iter().map(mailbox).collect())
}

/// The v3 Mail Send body for `message`.
fn request_body(message: &Message, config: &SendGridConfig) -> Vec<u8> {
  let mut personalization = Map::new();
  personalization.insert("to".into(), mailboxes(message.to()));
  if !message.cc().is_empty() {
    personalization.insert("cc".into(), mailboxes(message.cc()));
  }
  if !message.bcc().is_empty() {
    personalization.insert("bcc".into(), mailboxes(message.bcc()));
  }
  // The API wants text/plain before text/html.
  let mut content = Vec::new();
  if let Some(text) = message.text() {
    content.push(json!({ "type": "text/plain", "value": text }));
  }
  if let Some(html) = message.html() {
    content.push(json!({ "type": "text/html", "value": html }));
  }
  let mut body = Map::new();
  body.insert(
    "personalizations".into(),
    Value::Array(vec![Value::Object(personalization)]),
  );
  body.insert("from".into(), mailbox(message.from()));
  if let Some(reply_to) = message.reply_to() {
    body.insert("reply_to".into(), mailbox(reply_to));
  }
  body.insert("subject".into(), Value::from(message.subject()));
  body.insert("content".into(), Value::Array(content));
  let attachments = message
    .attachments()
    .iter()
    .map(|a| {
      json!({
        "content": STANDARD.encode(a.data()),
        "type": a.content_type(),
        "filename": a.filename(),
        "disposition": "attachment",
      })
    })
    .chain(message.inline().iter().map(|p| {
      json!({
        "content": STANDARD.encode(p.data()),
        "type": p.content_type(),
        "filename": p.filename().unwrap_or(p.content_id()),
        "disposition": "inline",
        "content_id": p.content_id(),
      })
    }))
    .collect::<Vec<_>>();
  if !attachments.is_empty() {
    body.insert("attachments".into(), Value::Array(attachments));
  }
  if !message.headers().is_empty() {
    let headers = message
      .headers()
      .iter()
      .map(|(k, v)| (k.clone(), Value::from(v.as_str())))
      .collect::<Map<_, _>>();
    body.insert("headers".into(), Value::Object(headers));
  }
  if config.sandbox {
    body.insert(
      "mail_settings".into(),
      json!({ "sandbox_mode": { "enable": true } }),
    );
  }
  Value::Object(body).to_string().into_bytes()
}

/// [`Sent`] for a `2xx` (SendGrid answers `202 Accepted`), else the [`MailError`] for the status.
fn sent(resp: &HttpResponse) -> Result<Sent, MailError> {
  let code = resp.status;
  if (200..300).contains(&code) {
    let id = resp
      .headers
      .iter()
      .find(|(k, _)| k.eq_ignore_ascii_case("x-message-id"))
      .map(|(_, v)| v.clone());
    return Ok(Sent { id });
  }
  let message = reason(&resp.body);
  Err(match code {
    401 | 403 => MailError::Unauthorized(message),
    429 | 500..=599 => MailError::Temporary { code, message },
    _ => MailError::Rejected { code, message },
  })
}

/// The `errors[].message` of a SendGrid error body (with the `field` when given), or the body
/// itself, cut to [`MAX_REASON_BYTES`].
fn reason(body: &[u8]) -> String {
  let parsed = serde_json::from_slice::<Value>(body).ok().and_then(|v| {
    let errors = v.get("errors")?.as_array()?;
    let messages = errors
      .iter()
      .filter_map(|e| {
        let message = e.get("message")?.as_str()?;
        Some(match e.get("field").and_then(Value::as_str) {
          Some(field) => format!("{field}: {message}"),
          None => message.to_owned(),
        })
      })
      .collect::<Vec<_>>();
    (!messages.is_empty()).then(|| messages.join("; "))
  });
  let mut out = parsed.unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
  if out.len() > MAX_REASON_BYTES {
    let mut end = MAX_REASON_BYTES;
    while !out.is_char_boundary(end) {
      end -= 1;
    }
    out.truncate(end);
  }
  out
}

fn http_error(e: HttpError) -> MailError {
  match e {
    HttpError::InvalidRequest(m) => MailError::Config(m),
    other => MailError::Transport(other.to_string()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Address, Attachment, InlinePart};

  fn message() -> Message {
    Message::builder()
      .from(Mailbox::named(
        "Ledger",
        Address::parse("billing@ledger.example").unwrap(),
      ))
      .to(Address::parse("carol@ledger.example").unwrap())
      .cc(Address::parse("dan@ledger.example").unwrap())
      .reply_to(Address::parse("help@ledger.example").unwrap())
      .subject("Invoice")
      .text("See attached.")
      .html("<img src=\"cid:logo\">")
      .attach(Attachment::new(
        "invoice.pdf",
        "application/pdf",
        b"%PDF".to_vec(),
      ))
      .inline(InlinePart::new("logo", "image/png", vec![137, 80]))
      .header("X-Entity-Ref-ID", "inv-1")
      .build()
      .unwrap()
  }

  fn body(config: &SendGridConfig) -> Value {
    serde_json::from_slice(&request_body(&message(), config)).unwrap()
  }

  #[test]
  fn the_body_follows_the_v3_shape() {
    let v = body(&SendGridConfig::new("k"));
    assert_eq!(
      v["personalizations"][0]["to"][0]["email"],
      "carol@ledger.example"
    );
    assert_eq!(
      v["personalizations"][0]["cc"][0]["email"],
      "dan@ledger.example"
    );
    assert!(v["personalizations"][0].get("bcc").is_none());
    assert_eq!(
      v["from"],
      json!({ "email": "billing@ledger.example", "name": "Ledger" })
    );
    assert_eq!(v["reply_to"]["email"], "help@ledger.example");
    assert_eq!(v["content"][0]["type"], "text/plain");
    assert_eq!(v["content"][1]["type"], "text/html");
    assert_eq!(v["attachments"][0]["content"], STANDARD.encode(b"%PDF"));
    assert_eq!(v["attachments"][0]["disposition"], "attachment");
    assert_eq!(v["attachments"][1]["disposition"], "inline");
    assert_eq!(v["attachments"][1]["content_id"], "logo");
    assert_eq!(v["attachments"][1]["filename"], "logo");
    assert_eq!(v["headers"]["X-Entity-Ref-ID"], "inv-1");
    assert!(v.get("mail_settings").is_none());
    let sandboxed = body(&SendGridConfig::new("k").sandbox(true));
    assert_eq!(sandboxed["mail_settings"]["sandbox_mode"]["enable"], true);
  }

  #[test]
  fn statuses_map_to_mail_errors() {
    let resp = |status: u16, body: &str| HttpResponse {
      status,
      headers: vec![("X-Message-Id".into(), "abc".into())],
      body: body.as_bytes().to_vec(),
    };
    assert_eq!(sent(&resp(202, "")).unwrap().id.as_deref(), Some("abc"));
    let errors = r#"{"errors":[{"message":"bad from","field":"from.email"},{"message":"x"}]}"#;
    assert_eq!(
      sent(&resp(400, errors)),
      Err(MailError::Rejected {
        code: 400,
        message: "from.email: bad from; x".into()
      })
    );
    assert!(matches!(
      sent(&resp(401, "")),
      Err(MailError::Unauthorized(_))
    ));
    assert!(matches!(
      sent(&resp(429, "slow down")),
      Err(MailError::Temporary { code: 429, message }) if message == "slow down"
    ));
    assert!(matches!(
      sent(&resp(503, "")),
      Err(MailError::Temporary { code: 503, .. })
    ));
  }

  #[test]
  fn long_error_bodies_are_cut() {
    let long = "é".repeat(600);
    let cut = reason(long.as_bytes());
    assert!(cut.len() <= MAX_REASON_BYTES);
    assert!(long.starts_with(&cut));
  }

  #[test]
  fn the_api_key_is_never_in_debug() {
    let config = SendGridConfig::new("SG.secret-key").base_url("http://localhost:1/");
    assert!(!format!("{config:?}").contains("SG.secret-key"));
    assert_eq!(config.send_url(), "http://localhost:1/v3/mail/send");
  }
}
