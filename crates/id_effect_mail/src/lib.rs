//! Outgoing mail for id_effect services: SMTP, the SendGrid v3 API, templates, and delivery through
//! `id_effect_jobs`.
//!
//! [`Address`] is a validated `local@domain` mailbox address: no whitespace or control characters,
//! a local part of at most [`Address::MAX_LOCAL_LEN`] bytes, a domain of non-empty dot-separated
//! labels, and at most [`Address::MAX_LEN`] bytes in all (RFC 5321).
//!
//! ```
//! use id_effect_mail::{Address, AddressError};
//!
//! let from = Address::parse("billing@ledger.example").unwrap();
//! assert_eq!(from.to_string(), "billing@ledger.example");
//! assert_eq!(Address::parse("billing"), Err(AddressError::MissingAt));
//! ```
//!
//! A [`Message`] is built and checked by [`Message::builder`]: sender, `To` / `Cc` / `Bcc`,
//! `Reply-To`, subject, text and HTML bodies, [`Attachment`]s, [`InlinePart`]s the HTML
//! references as `cid:<content-id>`, and extra headers. It is sent through the [`Mailer`]
//! capability ([`MailerService`], [`deliver`]), provided by one of three layers:
//!
//! - [`SmtpMailer::layer`]: pooled SMTP over lettre (implicit TLS, `STARTTLS` or plaintext).
//! - [`SendGridMailer::layer`]: the SendGrid v3 API over the `HttpClient` capability.
//! - [`TestMailbox::layer`]: records messages in memory, with `assert_sent_to`,
//!   `assert_subject`, `assert_contains`.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect_mail::{Address, MailerEnv, Message, TestMailbox, deliver};
//!
//! let message = Message::builder()
//!   .from(Address::parse("billing@ledger.example").unwrap())
//!   .to(Address::parse("carol@ledger.example").unwrap())
//!   .subject("Your invoice")
//!   .text("Your invoice is attached.")
//!   .build()
//!   .unwrap();
//!
//! let mailbox = TestMailbox::new();
//! let program = deliver::<MailerEnv, _>(message).provide_layer(mailbox.layer());
//! assert!(matches!(program.block_on(), Exit::Success(_)));
//! mailbox.assert_sent_to("carol@ledger.example");
//! mailbox.assert_subject("Your invoice");
//! ```
//!
//! [`Templates`] renders a mail's subject, text and HTML bodies from minijinja templates with
//! shared layouts and partials: HTML autoescaped, a missing variable an error
//! ([`TemplateError`]), the context any `Serialize` value. With feature `preview`,
//! `MailPreviews` serves registered previews at `/_mail/previews` (HTML, text and raw MIME),
//! re-reading the templates from disk on every request in a debug build.
//!
//! ```
//! use id_effect_mail::{Address, Templates};
//!
//! let templates = Templates::new()
//!   .with_template("layout.txt", "{% block body %}{% endblock %}\n-- Ledger").unwrap()
//!   .with_template("receipt.subject.txt", "Receipt {{ number }}").unwrap()
//!   .with_template(
//!     "receipt.txt",
//!     r#"{% extends "layout.txt" %}{% block body %}Paid: {{ amount }}{% endblock %}"#,
//!   )
//!   .unwrap();
//! let message = templates
//!   .message("receipt", &serde_json::json!({ "number": 7, "amount": "12.00 EUR" }))
//!   .unwrap()
//!   .from(Address::parse("billing@ledger.example").unwrap())
//!   .to(Address::parse("carol@ledger.example").unwrap())
//!   .build()
//!   .unwrap();
//! assert_eq!(message.subject(), "Receipt 7");
//! assert_eq!(message.text(), Some("Paid: 12.00 EUR\n-- Ledger"));
//! ```
//!
//! Neither transport retries a send by itself; a refused send says whether a later attempt may
//! succeed ([`MailError::is_transient`]). With feature `jobs`, `deliver_later` enqueues a message
//! as a `DeliverMail` job of `id_effect_jobs` (`deliver_later_in` with the caller's SQL
//! transaction, so it is sent only if that commits), and `register_mail_jobs` adds the handler
//! that sends it through the [`Mailer`]: a transient failure is retried with backoff, any other
//! makes the job `dead` with the error recorded.
//!
//! With feature `auth`, `AccountMails` implements `id_effect_auth`'s `AccountNotifier`: the
//! verification and password reset mails, rendered from templates and sent directly through the
//! [`Mailer`] with a short retry. They never go through `deliver_later`, whose job payload would
//! keep a usable token in the jobs table (see the `auth` module).
//!
//! Tier `integration` (ADR 0010 Amendment 6).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

#[cfg(feature = "auth")]
pub mod auth;
#[cfg(feature = "jobs")]
mod later;
mod mailer;
mod message;
#[cfg(feature = "preview")]
mod preview;
mod sendgrid;
mod smtp;
mod template;
mod testing;

#[cfg(feature = "auth")]
pub use auth::AccountMails;
#[cfg(feature = "jobs")]
pub use later::{DeliverMail, deliver_later, deliver_later_in, register_mail_jobs};
pub use mailer::{
  MailError, Mailer, MailerEnv, MailerService, Sent, deliver, mailer_env, provide_mailer,
};
pub use message::{Attachment, InlinePart, Mailbox, Message, MessageBuilder};
#[cfg(feature = "preview")]
pub use preview::{MailPreviews, PREVIEWS_PATH};
pub use sendgrid::{SendGridConfig, SendGridMailer};
pub use smtp::{SmtpConfig, SmtpMailer, SmtpSecurity};
pub use template::{RenderedMail, TemplateError, Templates};
pub use testing::TestMailbox;

/// A validated `local@domain` mailbox address.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Address {
  addr: String,
  at: usize,
}

impl Address {
  /// Longest address, in bytes (RFC 5321 path limit minus the angle brackets).
  pub const MAX_LEN: usize = 254;
  /// Longest local part, in bytes (RFC 5321).
  pub const MAX_LOCAL_LEN: usize = 64;

  /// Validates `addr` as a mailbox address.
  pub fn parse(addr: impl Into<String>) -> Result<Self, AddressError> {
    let addr = addr.into();
    if addr.chars().any(|c| c.is_whitespace() || c.is_control()) {
      return Err(AddressError::Whitespace);
    }
    let (local, domain) = addr.split_once('@').ok_or(AddressError::MissingAt)?;
    if local.is_empty() {
      return Err(AddressError::EmptyLocal);
    }
    if local.len() > Self::MAX_LOCAL_LEN {
      return Err(AddressError::LocalTooLong);
    }
    if domain.contains('@') || domain.split('.').any(str::is_empty) {
      return Err(AddressError::InvalidDomain);
    }
    if addr.len() > Self::MAX_LEN {
      return Err(AddressError::TooLong);
    }
    let at = local.len();
    Ok(Address { addr, at })
  }

  /// The part before the `@`.
  pub fn local(&self) -> &str {
    &self.addr[..self.at]
  }

  /// The part after the `@`.
  pub fn domain(&self) -> &str {
    &self.addr[self.at + 1..]
  }

  /// The whole address.
  pub fn as_str(&self) -> &str {
    &self.addr
  }
}

impl fmt::Display for Address {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.addr)
  }
}

/// Why a string is not a valid [`Address`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressError {
  /// It contains whitespace or a control character.
  Whitespace,
  /// It has no `@`.
  MissingAt,
  /// Nothing comes before the `@`.
  EmptyLocal,
  /// The local part is longer than [`Address::MAX_LOCAL_LEN`] bytes.
  LocalTooLong,
  /// The domain is empty, has an empty label (`a..b`, a leading or trailing dot) or another `@`.
  InvalidDomain,
  /// The address is longer than [`Address::MAX_LEN`] bytes.
  TooLong,
}

impl fmt::Display for AddressError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      AddressError::Whitespace => "mail address contains whitespace or a control character",
      AddressError::MissingAt => "mail address has no `@`",
      AddressError::EmptyLocal => "mail address has nothing before the `@`",
      AddressError::LocalTooLong => "mail address local part is longer than 64 bytes",
      AddressError::InvalidDomain => "mail address domain is not a dot-separated host name",
      AddressError::TooLong => "mail address is longer than 254 bytes",
    })
  }
}

impl std::error::Error for AddressError {}

/// Runs the book chapter on mail as a doctest (its blocks need features `preview` and `jobs`).
#[cfg(all(doctest, feature = "preview", feature = "jobs"))]
#[doc = include_str!("../../id_effect/book/src/part6/ch31-03-mail.md")]
struct BookMailDoctests;
