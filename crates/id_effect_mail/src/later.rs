//! Delivery through `id_effect_jobs` (feature `jobs`): the [`DeliverMail`] job, enqueued by
//! [`deliver_later`] and [`deliver_later_in`] and sent by the handler [`register_mail_jobs`]
//! adds to a [`JobRegistry`].
//!
//! The job's payload is the whole [`Message`] (attachment and inline content base64-encoded), so
//! a worker sends exactly what was enqueued; a [`MailError::is_transient`] failure is retried on
//! [`DeliverMail::retry`], any other is discarded (the job is `dead` with the error recorded).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use id_effect::{Effect, HList, Schedule, Schema, Selector, WallClock, fail};
use id_effect_jobs::{
  Enqueued, Job, JobError, JobFailure, JobOptions, JobQueueService, JobRegistry, enqueue,
  enqueue_in,
};
use id_effect_sql::SqlTransaction;

use crate::{Address, Attachment, InlinePart, MailError, Mailbox, MailerService, Message, deliver};

/// A [`Message`] to send in the background: the job [`deliver_later`] enqueues and the handler
/// [`register_mail_jobs`] registers sends through the [`MailerService`].
///
/// The payload stores the whole message as JSON in the queue (the `id_effect_jobs` table with
/// [`PgJobQueue`](id_effect_jobs::PgJobQueue)): recipients including `Bcc`, both bodies and the
/// attachment and inline content, base64-encoded. It stays there until the finished job is
/// pruned (`WorkerConfig::prune_after`, 7 days by default). The attachment and inline content
/// together may be at most [`MAX_CONTENT_BYTES`](Self::MAX_CONTENT_BYTES); keep larger files in
/// object storage and link to them.
///
/// It runs on the queue [`QUEUE`](Job::QUEUE) `mail` (serve it with
/// `Worker::queue(DeliverMail::QUEUE, n)`), at most 12 attempts, backing off exponentially from
/// 30 s to at most an hour (about five hours in all). Delivery is at least once: a worker that
/// dies after the transport accepted the message sends it again on the next attempt.
#[derive(Clone, Debug, PartialEq, Schema)]
pub struct DeliverMail {
  from: MailboxPayload,
  to: Vec<MailboxPayload>,
  cc: Vec<MailboxPayload>,
  bcc: Vec<MailboxPayload>,
  reply_to: Option<MailboxPayload>,
  subject: String,
  text: Option<String>,
  html: Option<String>,
  attachments: Vec<AttachmentPayload>,
  inline: Vec<InlinePayload>,
  headers: Vec<HeaderPayload>,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct MailboxPayload {
  name: Option<String>,
  address: String,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct AttachmentPayload {
  filename: String,
  content_type: String,
  data: String,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct InlinePayload {
  content_id: String,
  content_type: String,
  filename: Option<String>,
  data: String,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct HeaderPayload {
  name: String,
  value: String,
}

impl DeliverMail {
  /// The most bytes of attachment and inline content a job carries (2 MiB before base64, about
  /// 2.7 MiB in the payload).
  pub const MAX_CONTENT_BYTES: usize = 2 * 1024 * 1024;

  /// `message` as a job, or [`JobError::Encode`] when its attachment and inline content exceed
  /// [`MAX_CONTENT_BYTES`](Self::MAX_CONTENT_BYTES).
  pub fn new(message: Message) -> Result<Self, JobError> {
    let content: usize = message
      .attachments()
      .iter()
      .map(|a| a.data().len())
      .chain(message.inline().iter().map(|p| p.data().len()))
      .sum();
    if content > Self::MAX_CONTENT_BYTES {
      return Err(JobError::Encode(format!(
        "mail `{}`: {content} bytes of attachments and inline parts exceed the {} bytes a \
         `DeliverMail` job carries",
        message.subject(),
        Self::MAX_CONTENT_BYTES
      )));
    }
    Ok(Self {
      from: MailboxPayload::of(message.from()),
      to: message.to().iter().map(MailboxPayload::of).collect(),
      cc: message.cc().iter().map(MailboxPayload::of).collect(),
      bcc: message.bcc().iter().map(MailboxPayload::of).collect(),
      reply_to: message.reply_to().map(MailboxPayload::of),
      subject: message.subject().to_owned(),
      text: message.text().map(str::to_owned),
      html: message.html().map(str::to_owned),
      attachments: message
        .attachments()
        .iter()
        .map(|a| AttachmentPayload {
          filename: a.filename().to_owned(),
          content_type: a.content_type().to_owned(),
          data: STANDARD.encode(a.data()),
        })
        .collect(),
      inline: message
        .inline()
        .iter()
        .map(|p| InlinePayload {
          content_id: p.content_id().to_owned(),
          content_type: p.content_type().to_owned(),
          filename: p.filename().map(str::to_owned),
          data: STANDARD.encode(p.data()),
        })
        .collect(),
      headers: message
        .headers()
        .iter()
        .map(|(name, value)| HeaderPayload {
          name: name.clone(),
          value: value.clone(),
        })
        .collect(),
    })
  }

  /// The message again, checked by [`MessageBuilder::build`](crate::MessageBuilder::build);
  /// [`MailError::InvalidMessage`] for a payload that no longer makes a valid message.
  pub fn to_message(&self) -> Result<Message, MailError> {
    let mut builder = Message::builder()
      .from(self.from.to_mailbox()?)
      .subject(self.subject.clone());
    for m in &self.to {
      builder = builder.to(m.to_mailbox()?);
    }
    for m in &self.cc {
      builder = builder.cc(m.to_mailbox()?);
    }
    for m in &self.bcc {
      builder = builder.bcc(m.to_mailbox()?);
    }
    if let Some(m) = &self.reply_to {
      builder = builder.reply_to(m.to_mailbox()?);
    }
    if let Some(text) = &self.text {
      builder = builder.text(text.clone());
    }
    if let Some(html) = &self.html {
      builder = builder.html(html.clone());
    }
    for a in &self.attachments {
      let data = decode(&a.data, &a.filename)?;
      builder = builder.attach(Attachment::new(
        a.filename.clone(),
        a.content_type.clone(),
        data,
      ));
    }
    for p in &self.inline {
      let mut part = InlinePart::new(
        p.content_id.clone(),
        p.content_type.clone(),
        decode(&p.data, &p.content_id)?,
      );
      if let Some(name) = &p.filename {
        part = part.with_filename(name.clone());
      }
      builder = builder.inline(part);
    }
    for h in &self.headers {
      builder = builder.header(h.name.clone(), h.value.clone());
    }
    builder.build()
  }
}

impl MailboxPayload {
  fn of(mailbox: &Mailbox) -> Self {
    Self {
      name: mailbox.name().map(str::to_owned),
      address: mailbox.address().as_str().to_owned(),
    }
  }

  fn to_mailbox(&self) -> Result<Mailbox, MailError> {
    let address = Address::parse(self.address.clone())
      .map_err(|e| MailError::InvalidMessage(format!("address `{}`: {e}", self.address)))?;
    Ok(match &self.name {
      Some(name) => Mailbox::named(name.clone(), address),
      None => Mailbox::new(address),
    })
  }
}

/// The base64 content of the part `what`.
fn decode(data: &str, what: &str) -> Result<Vec<u8>, MailError> {
  STANDARD
    .decode(data)
    .map_err(|e| MailError::InvalidMessage(format!("content of `{what}` is not base64: {e}")))
}

impl Job for DeliverMail {
  const NAME: &'static str = "id_effect_mail.deliver";
  const QUEUE: &'static str = "mail";

  fn max_attempts(&self) -> u16 {
    12
  }

  /// Exponential from 30 s, capped at 1 h, jittered by ±20 %.
  fn retry() -> Schedule<(), JobFailure> {
    Schedule::exponential(Duration::from_secs(30))
      // `union` takes the shorter delay: at most one hour.
      .union(Schedule::spaced(Duration::from_secs(3600)))
      .jittered()
      .map(|_| ())
  }
}

/// A transient [`MailError`] ([`MailError::is_transient`]) is a [`JobFailure::Retry`]; any other
/// (rejected, unauthorized, an invalid message, a misconfigured transport) a
/// [`JobFailure::Discard`]: the job is `dead` at once, with the error as its `last_error`.
impl From<MailError> for JobFailure {
  fn from(error: MailError) -> Self {
    if error.is_transient() {
      JobFailure::retry(error)
    } else {
      JobFailure::discard(error)
    }
  }
}

/// Enqueues `message` as a [`DeliverMail`] job on the [`JobQueueService`] in `R`. Fails with
/// [`JobError::Encode`] when its content is too large ([`DeliverMail::new`]). For a delay or a
/// unique key, enqueue `DeliverMail::new(message)?` with [`JobOptions`] yourself.
pub fn deliver_later<R, I>(message: Message) -> Effect<Enqueued, JobError, R>
where
  R: Selector<JobQueueService, I> + HList,
{
  match DeliverMail::new(message) {
    Ok(job) => enqueue::<_, R, I>(job, JobOptions::new()),
    Err(e) => fail(e),
  }
}

/// Enqueues `message` as a [`DeliverMail`] job with the caller's transaction `tx`
/// ([`enqueue_in`]): it is sent only if `tx` commits. `clock` is the queue's clock.
pub fn deliver_later_in(
  tx: &Arc<dyn SqlTransaction>,
  message: Message,
  clock: &WallClock,
) -> Effect<Enqueued, JobError> {
  match DeliverMail::new(message) {
    Ok(job) => enqueue_in(tx, job, JobOptions::new(), clock),
    Err(e) => fail(e),
  }
}

/// Registers the [`DeliverMail`] handler in `registry`: it sends the message with the
/// [`MailerService`] in `R`, so the worker's environment holds both it and the
/// [`JobQueueService`]. A transient failure retries, any other discards (the `From<MailError>`
/// impl of [`JobFailure`]). Fails with [`JobError::DuplicateJob`] when the registry already has
/// it.
pub fn register_mail_jobs<R, I>(registry: JobRegistry<R>) -> Result<JobRegistry<R>, JobError>
where
  R: Selector<MailerService, I> + HList + Send + Sync + 'static,
  I: 'static,
{
  registry.register::<DeliverMail, MailError>(|job, _ctx| match job.to_message() {
    Ok(message) => deliver::<R, I>(message).map(|_| ()),
    Err(e) => fail(e),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn message() -> Message {
    let addr = |a: &str| Address::parse(a).unwrap();
    Message::builder()
      .from(Mailbox::named(
        "Ledger Billing",
        addr("billing@ledger.example"),
      ))
      .to(addr("carol@ledger.example"))
      .cc(addr("dan@ledger.example"))
      .bcc(addr("audit@ledger.example"))
      .reply_to(Mailbox::named("Support", addr("help@ledger.example")))
      .subject("Invoice 7")
      .text("Your invoice is attached.")
      .html(r#"<img src="cid:logo"> Your invoice is attached."#)
      .attach(Attachment::new(
        "invoice.pdf",
        "application/pdf",
        vec![0u8, 255, 37, 80],
      ))
      .inline(InlinePart::new("logo", "image/png", vec![137u8, 80, 78, 71]).with_filename("l.png"))
      .header("X-Entity-Ref-ID", "invoice-7")
      .build()
      .unwrap()
  }

  #[test]
  fn the_payload_round_trips_the_whole_message() {
    let job = DeliverMail::new(message()).unwrap();
    let schema = DeliverMail::schema();
    let decoded = schema.decode_unknown(&schema.encode(job.clone())).unwrap();
    assert_eq!(decoded, job);
    assert_eq!(decoded.to_message().unwrap(), message());
  }

  #[test]
  fn content_over_the_limit_is_refused() {
    let big = Message::builder()
      .from(Address::parse("billing@ledger.example").unwrap())
      .to(Address::parse("carol@ledger.example").unwrap())
      .text("Attached.")
      .attach(Attachment::new(
        "big.bin",
        "application/octet-stream",
        vec![0u8; DeliverMail::MAX_CONTENT_BYTES + 1],
      ))
      .build()
      .unwrap();
    let Err(JobError::Encode(why)) = DeliverMail::new(big) else {
      panic!("too large");
    };
    assert!(why.contains("exceed"), "{why}");
  }

  #[test]
  fn a_payload_that_no_longer_builds_is_an_invalid_message() {
    let mut job = DeliverMail::new(message()).unwrap();
    job.attachments[0].data = "not base64!".into();
    assert!(matches!(
      job.to_message(),
      Err(MailError::InvalidMessage(_))
    ));
    let mut job = DeliverMail::new(message()).unwrap();
    job.to[0].address = "carol".into();
    assert!(matches!(
      job.to_message(),
      Err(MailError::InvalidMessage(_))
    ));
  }

  #[test]
  fn transient_errors_retry_and_the_rest_discard() {
    let temporary = MailError::Temporary {
      code: 451,
      message: "greylisted".into(),
    };
    assert_eq!(
      JobFailure::from(temporary),
      JobFailure::Retry("mail temporarily refused (451): greylisted".into())
    );
    assert!(!JobFailure::from(MailError::Transport("reset".into())).is_discard());
    for permanent in [
      MailError::Rejected {
        code: 550,
        message: "no such user".into(),
      },
      MailError::InvalidMessage("no body".into()),
      MailError::Unauthorized("bad key".into()),
      MailError::Config("empty host".into()),
    ] {
      assert!(JobFailure::from(permanent).is_discard());
    }
    assert_eq!(
      (DeliverMail::NAME, DeliverMail::QUEUE),
      ("id_effect_mail.deliver", "mail")
    );
    let failure = JobFailure::retry("x");
    let delay = |n| id_effect_jobs::retry_delay(&DeliverMail::retry(), &failure, n).unwrap();
    assert!((24..=36).contains(&delay(1).as_secs()), "{:?}", delay(1));
    assert!(delay(11) <= Duration::from_secs(3600 * 6 / 5));
  }
}
