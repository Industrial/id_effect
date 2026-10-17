//! The [`Mailer`] capability: one `send` per [`Message`], the [`MailerService`] that carries an
//! implementation in the typed environment, and the typed [`MailError`].

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, hlist, need};

use crate::Message;

/// What a transport reports for an accepted message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sent {
  /// The transport's id for the message: the `Message-ID` header over SMTP, SendGrid's
  /// `X-Message-Id`, `test-<n>` from a [`TestMailbox`](crate::TestMailbox). `None` when the
  /// provider did not return one.
  pub id: Option<String>,
}

/// Why a message was not sent.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MailError {
  /// The message is malformed (no sender, no recipient, no body, a bad header or content type).
  InvalidMessage(String),
  /// The transport is misconfigured (an empty host, a TLS setup failure, a bad API URL).
  Config(String),
  /// The connection failed: refused, reset, timed out, TLS handshake, an unreadable reply.
  /// Whether the provider received the message is unknown.
  Transport(String),
  /// The credentials were refused (SMTP `530` / `534` / `535`, HTTP `401` / `403`).
  Unauthorized(String),
  /// The provider refused the message for good (SMTP `5xx`, HTTP `4xx`); sending it again
  /// fails the same way.
  Rejected {
    /// The SMTP reply code or HTTP status.
    code: u16,
    /// The provider's reason.
    message: String,
  },
  /// The provider refused the message for now (SMTP `4xx`, HTTP `429` / `5xx`); a later attempt
  /// may succeed.
  Temporary {
    /// The SMTP reply code or HTTP status.
    code: u16,
    /// The provider's reason.
    message: String,
  },
}

impl MailError {
  /// Whether a later attempt may succeed ([`MailError::Temporary`], [`MailError::Transport`]).
  /// A transport failure may also mean the provider got the message, so retrying it can deliver
  /// twice.
  pub fn is_transient(&self) -> bool {
    matches!(self, MailError::Temporary { .. } | MailError::Transport(_))
  }
}

impl fmt::Display for MailError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      MailError::InvalidMessage(m) => write!(f, "invalid mail message: {m}"),
      MailError::Config(m) => write!(f, "mail transport configuration: {m}"),
      MailError::Transport(m) => write!(f, "mail transport failed: {m}"),
      MailError::Unauthorized(m) => write!(f, "mail credentials refused: {m}"),
      MailError::Rejected { code, message } => write!(f, "mail rejected ({code}): {message}"),
      MailError::Temporary { code, message } => {
        write!(f, "mail temporarily refused ({code}): {message}")
      }
    }
  }
}

impl std::error::Error for MailError {}

/// Capability: sends one [`Message`] as an [`Effect`]. Implemented by
/// [`SmtpMailer`](crate::SmtpMailer), [`SendGridMailer`](crate::SendGridMailer) and
/// [`TestMailbox`](crate::TestMailbox).
pub trait Mailer: Send + Sync + 'static {
  /// Sends `message`; succeeds once the transport accepted it.
  fn send(&self, message: Message) -> Effect<Sent, MailError>;
}

/// The [`Mailer`] capability: a shared handle to the installed implementation.
#[derive(Clone)]
pub struct MailerService(pub Arc<dyn Mailer>);

impl MailerService {
  /// Wrap `mailer` as the capability.
  pub fn new(mailer: impl Mailer) -> Self {
    Self(Arc::new(mailer))
  }
}

impl std::ops::Deref for MailerService {
  type Target = dyn Mailer;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl fmt::Debug for MailerService {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("MailerService")
      .field(&"<dyn Mailer>")
      .finish()
  }
}

/// The minimal typed environment holding only [`MailerService`] (the output of the mailer
/// layers).
pub type MailerEnv = HCons<MailerService, HNil>;

/// A [`MailerEnv`] wrapping `mailer`.
pub fn mailer_env(mailer: impl Mailer) -> MailerEnv {
  hlist![MailerService::new(mailer)]
}

/// A [`Layer`] providing `mailer` as the [`MailerService`].
pub fn provide_mailer<E: Send + 'static>(mailer: impl Mailer) -> Layer<MailerEnv, E> {
  Layer::succeed(mailer_env(mailer))
}

/// Sends `message` with the [`MailerService`] in `R`.
pub fn deliver<R, I>(message: Message) -> Effect<Sent, MailError, R>
where
  R: Selector<MailerService, I> + HList,
{
  need::<MailerService>().flat_map(move |mailer| mailer.send(message).narrow())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn transient_errors_are_temporary_and_transport_failures() {
    let temporary = MailError::Temporary {
      code: 451,
      message: "try later".into(),
    };
    assert!(temporary.is_transient());
    assert!(MailError::Transport("reset".into()).is_transient());
    let rejected = MailError::Rejected {
      code: 550,
      message: "no such user".into(),
    };
    assert!(!rejected.is_transient());
    assert!(!MailError::Unauthorized("bad key".into()).is_transient());
    assert!(!MailError::InvalidMessage("no body".into()).is_transient());
  }

  #[test]
  fn errors_display_their_code_and_reason() {
    let e = MailError::Rejected {
      code: 550,
      message: "no such user".into(),
    };
    assert_eq!(e.to_string(), "mail rejected (550): no such user");
    assert!(
      MailError::Config("empty host".into())
        .to_string()
        .contains("empty host")
    );
  }
}
