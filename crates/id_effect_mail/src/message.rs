//! The mail model: [`Mailbox`], [`Message`] (validated by [`MessageBuilder::build`]),
//! [`Attachment`] and [`InlinePart`], and the MIME tree a message renders to
//! ([`Message::to_mime`]).
//!
//! The tree is the smallest one that holds the parts: `multipart/alternative` for a text and an
//! HTML body, `multipart/related` around the HTML and its inline parts (referenced from the HTML
//! as `cid:<content-id>`), and `multipart/mixed` around the body and the attachments.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment as MimeAttachment, MultiPart, MultiPartBuilder, SinglePart};

use crate::{Address, MailError};

/// Header names a message sets from its own fields; [`MessageBuilder::header`] refuses them.
const RESERVED_HEADERS: [&str; 13] = [
  "bcc",
  "cc",
  "content-disposition",
  "content-id",
  "content-transfer-encoding",
  "content-type",
  "date",
  "from",
  "message-id",
  "mime-version",
  "reply-to",
  "subject",
  "to",
];

/// A mailbox: an [`Address`] with an optional display name (`Carol <carol@ledger.example>`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Mailbox {
  name: Option<String>,
  address: Address,
}

impl Mailbox {
  /// `address` without a display name.
  pub fn new(address: Address) -> Self {
    Self {
      name: None,
      address,
    }
  }

  /// `address` shown as `name`.
  pub fn named(name: impl Into<String>, address: Address) -> Self {
    Self {
      name: Some(name.into()),
      address,
    }
  }

  /// The display name.
  pub fn name(&self) -> Option<&str> {
    self.name.as_deref()
  }

  /// The address.
  pub fn address(&self) -> &Address {
    &self.address
  }

  fn to_lettre(&self) -> Result<lettre::message::Mailbox, MailError> {
    let address = lettre::Address::new(self.address.local(), self.address.domain())
      .map_err(|e| invalid(format!("address `{}`: {e}", self.address)))?;
    Ok(lettre::message::Mailbox::new(self.name.clone(), address))
  }

  fn validate(&self) -> Result<(), MailError> {
    if let Some(name) = &self.name {
      no_line_breaks("a display name", name)?;
    }
    self.to_lettre().map(|_| ())
  }
}

impl From<Address> for Mailbox {
  fn from(address: Address) -> Self {
    Self::new(address)
  }
}

impl fmt::Display for Mailbox {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match &self.name {
      Some(name) => write!(f, "{name} <{}>", self.address),
      None => write!(f, "{}", self.address),
    }
  }
}

/// A file attached to a message (`Content-Disposition: attachment`).
#[derive(Clone, PartialEq, Eq)]
pub struct Attachment {
  filename: String,
  content_type: String,
  data: Vec<u8>,
}

impl Attachment {
  /// `data` attached as `filename`, of MIME type `content_type` (`application/pdf`).
  pub fn new(
    filename: impl Into<String>,
    content_type: impl Into<String>,
    data: impl Into<Vec<u8>>,
  ) -> Self {
    Self {
      filename: filename.into(),
      content_type: content_type.into(),
      data: data.into(),
    }
  }

  /// The file name shown to the reader.
  pub fn filename(&self) -> &str {
    &self.filename
  }

  /// The MIME type.
  pub fn content_type(&self) -> &str {
    &self.content_type
  }

  /// The content.
  pub fn data(&self) -> &[u8] {
    &self.data
  }

  fn validate(&self) -> Result<(), MailError> {
    if self.filename.trim().is_empty() {
      return Err(invalid("an attachment has an empty file name"));
    }
    no_line_breaks("an attachment file name", &self.filename)?;
    content_type(&self.content_type).map(|_| ())
  }

  fn to_part(&self) -> Result<SinglePart, MailError> {
    let content_type = content_type(&self.content_type)?;
    Ok(MimeAttachment::new(self.filename.clone()).body(self.data.clone(), content_type))
  }
}

impl fmt::Debug for Attachment {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Attachment")
      .field("filename", &self.filename)
      .field("content_type", &self.content_type)
      .field("len", &self.data.len())
      .finish()
  }
}

/// A part shown inside the HTML body (`Content-Disposition: inline`), such as an image the HTML
/// references as `<img src="cid:logo">`.
#[derive(Clone, PartialEq, Eq)]
pub struct InlinePart {
  content_id: String,
  content_type: String,
  data: Vec<u8>,
  filename: Option<String>,
}

impl InlinePart {
  /// `data` of MIME type `content_type` (`image/png`), referenced from the HTML as
  /// `cid:<content_id>`.
  pub fn new(
    content_id: impl Into<String>,
    content_type: impl Into<String>,
    data: impl Into<Vec<u8>>,
  ) -> Self {
    Self {
      content_id: content_id.into(),
      content_type: content_type.into(),
      data: data.into(),
      filename: None,
    }
  }

  /// Names the part `filename` (a reader saving it sees this name; SendGrid needs one and
  /// defaults to the content id).
  pub fn with_filename(mut self, filename: impl Into<String>) -> Self {
    self.filename = Some(filename.into());
    self
  }

  /// The content id (without the angle brackets of the `Content-ID` header).
  pub fn content_id(&self) -> &str {
    &self.content_id
  }

  /// The MIME type.
  pub fn content_type(&self) -> &str {
    &self.content_type
  }

  /// The content.
  pub fn data(&self) -> &[u8] {
    &self.data
  }

  /// The file name, if one was given.
  pub fn filename(&self) -> Option<&str> {
    self.filename.as_deref()
  }

  fn validate(&self) -> Result<(), MailError> {
    let id = &self.content_id;
    if id.is_empty()
      || id
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || "<>".contains(c))
    {
      return Err(invalid(format!(
        "inline content id {id:?} is empty or has whitespace or angle brackets"
      )));
    }
    if let Some(name) = &self.filename {
      no_line_breaks("an inline part file name", name)?;
    }
    content_type(&self.content_type).map(|_| ())
  }

  fn to_part(&self) -> Result<SinglePart, MailError> {
    let content_type = content_type(&self.content_type)?;
    let disposition = match &self.filename {
      Some(name) => MimeAttachment::new_inline_with_name(self.content_id.clone(), name.clone()),
      None => MimeAttachment::new_inline(self.content_id.clone()),
    };
    Ok(disposition.body(self.data.clone(), content_type))
  }
}

impl fmt::Debug for InlinePart {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("InlinePart")
      .field("content_id", &self.content_id)
      .field("content_type", &self.content_type)
      .field("filename", &self.filename)
      .field("len", &self.data.len())
      .finish()
  }
}

/// An outgoing message, valid by construction ([`Message::builder`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
  from: Mailbox,
  to: Vec<Mailbox>,
  cc: Vec<Mailbox>,
  bcc: Vec<Mailbox>,
  reply_to: Option<Mailbox>,
  subject: String,
  text: Option<String>,
  html: Option<String>,
  attachments: Vec<Attachment>,
  inline: Vec<InlinePart>,
  headers: Vec<(String, String)>,
}

impl Message {
  /// An empty builder.
  pub fn builder() -> MessageBuilder {
    MessageBuilder::default()
  }

  /// The sender.
  pub fn from(&self) -> &Mailbox {
    &self.from
  }

  /// The `To` recipients.
  pub fn to(&self) -> &[Mailbox] {
    &self.to
  }

  /// The `Cc` recipients.
  pub fn cc(&self) -> &[Mailbox] {
    &self.cc
  }

  /// The `Bcc` recipients (in the envelope only, never in the headers sent).
  pub fn bcc(&self) -> &[Mailbox] {
    &self.bcc
  }

  /// Every recipient: `To`, then `Cc`, then `Bcc`.
  pub fn recipients(&self) -> impl Iterator<Item = &Mailbox> {
    self.to.iter().chain(&self.cc).chain(&self.bcc)
  }

  /// The `Reply-To` mailbox.
  pub fn reply_to(&self) -> Option<&Mailbox> {
    self.reply_to.as_ref()
  }

  /// The subject (empty when none was set).
  pub fn subject(&self) -> &str {
    &self.subject
  }

  /// The plain-text body.
  pub fn text(&self) -> Option<&str> {
    self.text.as_deref()
  }

  /// The HTML body.
  pub fn html(&self) -> Option<&str> {
    self.html.as_deref()
  }

  /// The attachments, in order.
  pub fn attachments(&self) -> &[Attachment] {
    &self.attachments
  }

  /// The inline parts of the HTML body, in order.
  pub fn inline(&self) -> &[InlinePart] {
    &self.inline
  }

  /// The extra headers, in order.
  pub fn headers(&self) -> &[(String, String)] {
    &self.headers
  }

  /// The value of the extra header `name` (case-insensitive).
  pub fn header(&self, name: &str) -> Option<&str> {
    self
      .headers
      .iter()
      .find(|(n, _)| n.eq_ignore_ascii_case(name))
      .map(|(_, v)| v.as_str())
  }

  /// The message as RFC 5322 bytes, with a new `Message-ID` and the current `Date` (`Bcc` is
  /// left out, as it is when sent).
  pub fn to_mime(&self) -> Result<Vec<u8>, MailError> {
    Ok(self.to_lettre(&self.new_message_id())?.formatted())
  }

  /// A new `Message-ID` value (`<…@sender-domain>`), unique within the process.
  pub(crate) fn new_message_id(&self) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |d| d.as_nanos());
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!(
      "<{nanos:x}.{:x}.{n:x}@{}>",
      std::process::id(),
      self.from.address.domain()
    )
  }

  /// The lettre message with `message_id`, ready for an SMTP transport.
  pub(crate) fn to_lettre(&self, message_id: &str) -> Result<lettre::Message, MailError> {
    let mut builder = lettre::Message::builder()
      .from(self.from.to_lettre()?)
      .message_id(Some(message_id.to_owned()));
    for m in &self.to {
      builder = builder.to(m.to_lettre()?);
    }
    for m in &self.cc {
      builder = builder.cc(m.to_lettre()?);
    }
    for m in &self.bcc {
      builder = builder.bcc(m.to_lettre()?);
    }
    if let Some(m) = &self.reply_to {
      builder = builder.reply_to(m.to_lettre()?);
    }
    if !self.subject.is_empty() {
      builder = builder.subject(self.subject.clone());
    }
    for (name, value) in &self.headers {
      builder = builder.raw_header(HeaderValue::new(header_name(name)?, value.clone()));
    }
    let built = match self.mime_body()? {
      Part::Single(part) => builder.singlepart(part),
      Part::Multi(part) => builder.multipart(part),
    };
    built.map_err(|e| invalid(e.to_string()))
  }

  /// The body tree: alternative(text, related(html, inline…)) inside mixed(…, attachments…).
  fn mime_body(&self) -> Result<Part, MailError> {
    let html = match &self.html {
      None => None,
      Some(html) if self.inline.is_empty() => Some(Part::Single(SinglePart::html(html.clone()))),
      Some(html) => {
        let mut related = MultiPart::related().singlepart(SinglePart::html(html.clone()));
        for part in &self.inline {
          related = related.singlepart(part.to_part()?);
        }
        Some(Part::Multi(related))
      }
    };
    let text = self
      .text
      .clone()
      .map(|t| Part::Single(SinglePart::plain(t)));
    let body = match (text, html) {
      (Some(text), Some(html)) => Part::Multi(text.start(MultiPart::alternative()).with(html)),
      (Some(only), None) | (None, Some(only)) => only,
      (None, None) => return Err(invalid("a message needs a text or an HTML body")),
    };
    if self.attachments.is_empty() {
      return Ok(body);
    }
    let mut mixed = body.start(MultiPart::mixed());
    for attachment in &self.attachments {
      mixed = mixed.singlepart(attachment.to_part()?);
    }
    Ok(Part::Multi(mixed))
  }
}

/// A node of the MIME tree.
enum Part {
  Single(SinglePart),
  Multi(MultiPart),
}

impl Part {
  /// A multipart of `builder`'s kind starting with this part.
  fn start(self, builder: MultiPartBuilder) -> MultiPart {
    match self {
      Part::Single(p) => builder.singlepart(p),
      Part::Multi(p) => builder.multipart(p),
    }
  }
}

trait With {
  fn with(self, part: Part) -> Self;
}

impl With for MultiPart {
  fn with(self, part: Part) -> Self {
    match part {
      Part::Single(p) => self.singlepart(p),
      Part::Multi(p) => self.multipart(p),
    }
  }
}

/// Builds a [`Message`]; [`build`](Self::build) checks it.
#[derive(Clone, Debug, Default)]
pub struct MessageBuilder {
  from: Option<Mailbox>,
  to: Vec<Mailbox>,
  cc: Vec<Mailbox>,
  bcc: Vec<Mailbox>,
  reply_to: Option<Mailbox>,
  subject: String,
  text: Option<String>,
  html: Option<String>,
  attachments: Vec<Attachment>,
  inline: Vec<InlinePart>,
  headers: Vec<(String, String)>,
}

impl MessageBuilder {
  /// Sets the sender.
  pub fn from(mut self, from: impl Into<Mailbox>) -> Self {
    self.from = Some(from.into());
    self
  }

  /// Adds a `To` recipient.
  pub fn to(mut self, to: impl Into<Mailbox>) -> Self {
    self.to.push(to.into());
    self
  }

  /// Adds a `Cc` recipient.
  pub fn cc(mut self, cc: impl Into<Mailbox>) -> Self {
    self.cc.push(cc.into());
    self
  }

  /// Adds a `Bcc` recipient.
  pub fn bcc(mut self, bcc: impl Into<Mailbox>) -> Self {
    self.bcc.push(bcc.into());
    self
  }

  /// Sets the `Reply-To` mailbox.
  pub fn reply_to(mut self, reply_to: impl Into<Mailbox>) -> Self {
    self.reply_to = Some(reply_to.into());
    self
  }

  /// Sets the subject.
  pub fn subject(mut self, subject: impl Into<String>) -> Self {
    self.subject = subject.into();
    self
  }

  /// Sets the plain-text body.
  pub fn text(mut self, text: impl Into<String>) -> Self {
    self.text = Some(text.into());
    self
  }

  /// Sets the HTML body.
  pub fn html(mut self, html: impl Into<String>) -> Self {
    self.html = Some(html.into());
    self
  }

  /// Adds an attachment.
  pub fn attach(mut self, attachment: Attachment) -> Self {
    self.attachments.push(attachment);
    self
  }

  /// Adds an inline part of the HTML body.
  pub fn inline(mut self, part: InlinePart) -> Self {
    self.inline.push(part);
    self
  }

  /// Adds the header `name: value` (`List-Unsubscribe`, `X-Entity-Ref-ID`). Headers the message
  /// sets from its own fields (`From`, `Subject`, `Content-Type`, …) are refused by
  /// [`build`](Self::build).
  pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
    self.headers.push((name.into(), value.into()));
    self
  }

  /// The message, or [`MailError::InvalidMessage`] when it has no sender, no recipient or no
  /// body, inline parts without an HTML body, a line break in the subject, a display name, a
  /// file name or a header value, a content type that does not parse, an inline content id that
  /// is empty, repeated or has whitespace or angle brackets, or a reserved or malformed header
  /// name.
  pub fn build(self) -> Result<Message, MailError> {
    let from = self
      .from
      .ok_or_else(|| invalid("a message needs a `from` mailbox"))?;
    if self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty() {
      return Err(invalid("a message needs at least one recipient"));
    }
    if self.text.is_none() && self.html.is_none() {
      return Err(invalid("a message needs a text or an HTML body"));
    }
    if !self.inline.is_empty() && self.html.is_none() {
      return Err(invalid("inline parts need an HTML body to reference them"));
    }
    no_line_breaks("the subject", &self.subject)?;
    let mailboxes = std::iter::once(&from)
      .chain(&self.to)
      .chain(&self.cc)
      .chain(&self.bcc)
      .chain(&self.reply_to);
    for mailbox in mailboxes {
      mailbox.validate()?;
    }
    for attachment in &self.attachments {
      attachment.validate()?;
    }
    for (i, part) in self.inline.iter().enumerate() {
      part.validate()?;
      if self.inline[..i]
        .iter()
        .any(|p| p.content_id == part.content_id)
      {
        return Err(invalid(format!(
          "inline content id `{}` is used twice",
          part.content_id
        )));
      }
    }
    for (name, value) in &self.headers {
      header_name(name)?;
      if RESERVED_HEADERS
        .iter()
        .any(|r| r.eq_ignore_ascii_case(name))
      {
        return Err(invalid(format!(
          "header `{name}` is set from the message's own fields"
        )));
      }
      no_line_breaks("a header value", value)?;
    }
    Ok(Message {
      from,
      to: self.to,
      cc: self.cc,
      bcc: self.bcc,
      reply_to: self.reply_to,
      subject: self.subject,
      text: self.text,
      html: self.html,
      attachments: self.attachments,
      inline: self.inline,
      headers: self.headers,
    })
  }
}

fn invalid(message: impl Into<String>) -> MailError {
  MailError::InvalidMessage(message.into())
}

fn no_line_breaks(what: &str, value: &str) -> Result<(), MailError> {
  if value.contains(['\r', '\n']) {
    return Err(invalid(format!("{what} contains a line break")));
  }
  Ok(())
}

fn content_type(value: &str) -> Result<ContentType, MailError> {
  ContentType::parse(value).map_err(|e| invalid(format!("content type {value:?}: {e}")))
}

fn header_name(name: &str) -> Result<HeaderName, MailError> {
  let printable = |c: char| c.is_ascii_graphic() && c != ':';
  if name.is_empty() || !name.chars().all(printable) {
    return Err(invalid(format!(
      "header name {name:?} is not printable ASCII"
    )));
  }
  HeaderName::new_from_ascii(name.to_owned())
    .map_err(|_| invalid(format!("header name {name:?} is not valid")))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn addr(s: &str) -> Address {
    Address::parse(s).unwrap()
  }

  fn base() -> MessageBuilder {
    Message::builder()
      .from(addr("billing@ledger.example"))
      .to(addr("carol@ledger.example"))
  }

  #[test]
  fn build_requires_a_sender_a_recipient_and_a_body() {
    let no_from = Message::builder()
      .to(addr("carol@ledger.example"))
      .text("hi");
    assert!(matches!(no_from.build(), Err(MailError::InvalidMessage(m)) if m.contains("from")));
    let no_rcpt = Message::builder()
      .from(addr("billing@ledger.example"))
      .text("hi");
    assert!(
      matches!(no_rcpt.build(), Err(MailError::InvalidMessage(m)) if m.contains("recipient"))
    );
    assert!(matches!(base().build(), Err(MailError::InvalidMessage(m)) if m.contains("body")));
    assert!(base().text("hi").build().is_ok());
  }

  #[test]
  fn build_refuses_header_injection_and_reserved_headers() {
    let cases = [
      base().text("x").subject("a\r\nBcc: eve@evil.example"),
      base().text("x").header("Subject", "again"),
      base().text("x").header("X-Ok", "a\nb"),
      base().text("x").header("Bad Name", "v"),
      base().text("x").header("", "v"),
    ];
    for case in cases {
      assert!(matches!(case.build(), Err(MailError::InvalidMessage(_))));
    }
    let ok = base()
      .text("x")
      .header("List-Unsubscribe", "<mailto:u@x.example>");
    assert_eq!(
      ok.build().unwrap().header("list-unsubscribe"),
      Some("<mailto:u@x.example>")
    );
  }

  #[test]
  fn build_checks_parts() {
    let png = InlinePart::new("logo", "image/png", vec![1, 2, 3]);
    let no_html = base().text("x").inline(png.clone());
    assert!(matches!(no_html.build(), Err(MailError::InvalidMessage(m)) if m.contains("HTML")));
    let twice = base()
      .html("<img src=\"cid:logo\">")
      .inline(png.clone())
      .inline(png);
    assert!(matches!(twice.build(), Err(MailError::InvalidMessage(m)) if m.contains("twice")));
    let bad_cid = base()
      .html("x")
      .inline(InlinePart::new("a b", "image/png", vec![]));
    assert!(bad_cid.build().is_err());
    let bad_type = base()
      .text("x")
      .attach(Attachment::new("a.bin", "nonsense", vec![]));
    assert!(bad_type.build().is_err());
    let no_name = base()
      .text("x")
      .attach(Attachment::new(" ", "text/plain", vec![]));
    assert!(no_name.build().is_err());
  }

  #[test]
  fn mime_of_a_text_only_message_is_a_single_part() {
    let mime = base()
      .subject("Hi")
      .text("Hello")
      .build()
      .unwrap()
      .to_mime()
      .unwrap();
    let mime = String::from_utf8(mime).unwrap();
    assert!(mime.contains("Subject: Hi\r\n"));
    assert!(mime.contains("Content-Type: text/plain; charset=utf-8"));
    assert!(mime.contains("Message-ID: <"));
    assert!(mime.contains("@ledger.example>"));
    assert!(!mime.contains("multipart/"));
  }

  #[test]
  fn bcc_is_kept_out_of_the_headers() {
    let message = base()
      .bcc(addr("audit@ledger.example"))
      .text("x")
      .build()
      .unwrap();
    let mime = String::from_utf8(message.to_mime().unwrap()).unwrap();
    assert!(!mime.contains("audit@ledger.example"));
    let envelope = message.to_lettre("<id@ledger.example>").unwrap();
    let rcpt: Vec<String> = envelope
      .envelope()
      .to()
      .iter()
      .map(ToString::to_string)
      .collect();
    assert!(rcpt.contains(&"audit@ledger.example".to_owned()));
  }

  #[test]
  fn mailbox_displays_with_its_name() {
    let m = Mailbox::named("Carol", addr("carol@ledger.example"));
    assert_eq!(m.to_string(), "Carol <carol@ledger.example>");
    assert_eq!(
      Mailbox::from(addr("a@b.example")).to_string(),
      "a@b.example"
    );
  }

  #[test]
  fn debug_of_parts_shows_lengths_not_bytes() {
    let a = Attachment::new("a.pdf", "application/pdf", vec![0; 4096]);
    let s = format!("{a:?}");
    assert!(s.contains("len: 4096"));
    assert!(s.len() < 200);
  }
}
