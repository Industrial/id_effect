//! [`TestMailbox`]: an in-memory [`Mailer`] that records every message, with assertions for
//! tests.
//!
//! Provide it with [`TestMailbox::layer`] and keep a clone: every clone sees the same mailbox.
//! The `assert_*` methods panic with the messages that were sent, so a failing test shows what
//! went out instead.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use id_effect::{Effect, Layer, fail, succeed, suspend};

use crate::{MailError, Mailer, MailerEnv, Message, Sent, provide_mailer};

/// An in-memory [`Mailer`] for tests. Cloning shares the mailbox.
#[derive(Clone, Default)]
pub struct TestMailbox {
  state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
  sent: Vec<Message>,
  failures: VecDeque<MailError>,
  attempts: u64,
}

impl TestMailbox {
  /// An empty mailbox.
  pub fn new() -> Self {
    Self::default()
  }

  /// A [`Layer`] providing this mailbox as the [`MailerService`](crate::MailerService).
  pub fn layer<E: Send + 'static>(&self) -> Layer<MailerEnv, E> {
    provide_mailer(self.clone())
  }

  fn state(&self) -> MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The next send fails with `error` (queued: call it again to fail more sends in order). A
  /// failed send records nothing.
  pub fn fail_next(&self, error: MailError) {
    self.state().failures.push_back(error);
  }

  /// Every message sent, oldest first.
  pub fn sent(&self) -> Vec<Message> {
    self.state().sent.clone()
  }

  /// How many messages were sent.
  pub fn len(&self) -> usize {
    self.state().sent.len()
  }

  /// Whether nothing was sent.
  pub fn is_empty(&self) -> bool {
    self.state().sent.is_empty()
  }

  /// The last message sent.
  pub fn last(&self) -> Option<Message> {
    self.state().sent.last().cloned()
  }

  /// Forgets every sent message and queued failure.
  pub fn clear(&self) {
    let mut state = self.state();
    state.sent.clear();
    state.failures.clear();
  }

  /// The messages with `address` among their `To`, `Cc` or `Bcc` recipients.
  pub fn sent_to(&self, address: &str) -> Vec<Message> {
    self.matching(|m| m.recipients().any(|r| r.address().as_str() == address))
  }

  fn matching(&self, pred: impl Fn(&Message) -> bool) -> Vec<Message> {
    self
      .state()
      .sent
      .iter()
      .filter(|m| pred(m))
      .cloned()
      .collect()
  }

  #[track_caller]
  fn first_or_panic(&self, what: String, pred: impl Fn(&Message) -> bool) -> Message {
    let sent = self.sent();
    match sent.iter().find(|m| pred(m)) {
      Some(m) => m.clone(),
      None => panic!(
        "TestMailbox: no message {what}; sent {}:{}",
        sent.len(),
        summary(&sent)
      ),
    }
  }

  /// The first message sent to `address` (`To`, `Cc` or `Bcc`).
  ///
  /// # Panics
  ///
  /// If none was, listing what was sent.
  #[track_caller]
  pub fn assert_sent_to(&self, address: &str) -> Message {
    self.first_or_panic(format!("sent to {address}"), |m| {
      m.recipients().any(|r| r.address().as_str() == address)
    })
  }

  /// The first message whose subject is `subject`.
  ///
  /// # Panics
  ///
  /// If none was sent, listing what was.
  #[track_caller]
  pub fn assert_subject(&self, subject: &str) -> Message {
    self.first_or_panic(format!("with subject {subject:?}"), |m| {
      m.subject() == subject
    })
  }

  /// The first message whose text or HTML body contains `needle`.
  ///
  /// # Panics
  ///
  /// If none was sent, listing what was.
  #[track_caller]
  pub fn assert_contains(&self, needle: &str) -> Message {
    self.first_or_panic(format!("containing {needle:?}"), |m| {
      m.text().is_some_and(|t| t.contains(needle)) || m.html().is_some_and(|h| h.contains(needle))
    })
  }

  /// Checks that exactly `count` messages were sent.
  ///
  /// # Panics
  ///
  /// If another number was, listing them.
  #[track_caller]
  pub fn assert_count(&self, count: usize) {
    let sent = self.sent();
    assert!(
      sent.len() == count,
      "TestMailbox: expected {count} message(s), sent {}:{}",
      sent.len(),
      summary(&sent)
    );
  }

  /// Checks that nothing was sent.
  ///
  /// # Panics
  ///
  /// If something was, listing it.
  #[track_caller]
  pub fn assert_none_sent(&self) {
    self.assert_count(0);
  }
}

/// One line per message: recipients and subject.
fn summary(sent: &[Message]) -> String {
  sent
    .iter()
    .map(|m| {
      let to = m
        .recipients()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
      format!("\n  to [{to}] subject {:?}", m.subject())
    })
    .collect()
}

impl fmt::Debug for TestMailbox {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let state = self.state();
    f.debug_struct("TestMailbox")
      .field("sent", &state.sent.len())
      .field("queued_failures", &state.failures.len())
      .finish()
  }
}

impl Mailer for TestMailbox {
  fn send(&self, message: Message) -> Effect<Sent, MailError> {
    let mailbox = self.clone();
    suspend(move || {
      let mut state = mailbox.state();
      state.attempts += 1;
      if let Some(error) = state.failures.pop_front() {
        return fail(error);
      }
      let id = format!("test-{}", state.attempts);
      state.sent.push(message);
      succeed(Sent { id: Some(id) })
    })
  }
}

#[cfg(test)]
mod tests {
  use id_effect::Exit;

  use super::*;
  use crate::Address;

  fn message(to: &str, subject: &str) -> Message {
    Message::builder()
      .from(Address::parse("billing@ledger.example").unwrap())
      .to(Address::parse(to).unwrap())
      .subject(subject)
      .text(format!("Hello {to}"))
      .build()
      .unwrap()
  }

  fn send(mailbox: &TestMailbox, m: Message) -> Exit<Sent, MailError> {
    mailbox.send(m).block_on()
  }

  #[test]
  fn records_and_finds_messages() {
    let mailbox = TestMailbox::new();
    mailbox.assert_none_sent();
    let first = send(&mailbox, message("carol@ledger.example", "Invoice"));
    assert!(matches!(first, Exit::Success(Sent { id: Some(id) }) if id == "test-1"));
    send(&mailbox, message("dan@ledger.example", "Receipt"));
    mailbox.assert_count(2);
    assert_eq!(
      mailbox.assert_sent_to("dan@ledger.example").subject(),
      "Receipt"
    );
    assert_eq!(
      mailbox.assert_subject("Invoice").to()[0].address().as_str(),
      "carol@ledger.example"
    );
    mailbox.assert_contains("Hello dan");
    assert_eq!(mailbox.sent_to("carol@ledger.example").len(), 1);
    assert_eq!(mailbox.last().unwrap().subject(), "Receipt");
    mailbox.clear();
    assert!(mailbox.is_empty());
  }

  #[test]
  fn queued_failures_fail_sends_in_order() {
    let mailbox = TestMailbox::new();
    mailbox.fail_next(MailError::Transport("down".into()));
    let exit = send(&mailbox, message("carol@ledger.example", "Invoice"));
    assert!(matches!(exit, Exit::Failure(_)));
    assert!(mailbox.is_empty());
    send(&mailbox, message("carol@ledger.example", "Invoice"));
    assert_eq!(mailbox.len(), 1);
  }

  #[test]
  #[should_panic(expected = "no message sent to eve@ledger.example")]
  fn a_missing_recipient_panics_with_what_was_sent() {
    let mailbox = TestMailbox::new();
    send(&mailbox, message("carol@ledger.example", "Invoice"));
    mailbox.assert_sent_to("eve@ledger.example");
  }

  #[test]
  #[should_panic(expected = "expected 2 message(s), sent 1")]
  fn a_wrong_count_panics() {
    let mailbox = TestMailbox::new();
    send(&mailbox, message("carol@ledger.example", "Invoice"));
    mailbox.assert_count(2);
  }
}
