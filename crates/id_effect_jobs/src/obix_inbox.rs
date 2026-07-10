//! Idempotent consumer inbox backed by [obix](https://docs.rs/obix) + [job](https://docs.rs/job).

use id_effect::Effect;
use job::Jobs;
use obix::prelude::es_entity;
use obix::{Inbox, InboxConfig, InboxError as ObixInboxError, InboxHandler, InboxIdempotencyKey};
use serde::Serialize;
use sqlx::PgPool;

use crate::error::JobError;

/// Result of persisting an inbound message with idempotency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InboxPersistResult {
  /// First time this idempotency key was seen; job queued.
  Queued,
  /// Duplicate delivery — no new work scheduled.
  AlreadyApplied,
}

fn map_inbox_err(err: ObixInboxError) -> JobError {
  JobError::Storage(err.to_string())
}

/// obix inbox wired to the shared [`PgPool`] and [`job::Jobs`] poller.
#[derive(Clone)]
pub struct ObixInbox {
  inner: Inbox,
}

impl ObixInbox {
  /// Register inbox handlers on `jobs` and return a handle for enqueueing inbound events.
  pub fn install<H>(pool: &PgPool, jobs: &mut Jobs, config: InboxConfig, handler: H) -> Self
  where
    H: InboxHandler,
  {
    Self {
      inner: Inbox::new(pool, jobs, config, handler),
    }
  }

  /// Borrow the underlying obix inbox (advanced: custom `begin_op` flows).
  pub fn obix(&self) -> &Inbox {
    &self.inner
  }

  /// Persist an inbound event idempotently and queue processing.
  pub fn persist<P>(
    &self,
    idempotency_key: impl Into<InboxIdempotencyKey>,
    event: P,
  ) -> Effect<InboxPersistResult, JobError, ()>
  where
    P: Serialize + Send + Sync + 'static,
  {
    let inbox = self.inner.clone();
    let key = idempotency_key.into();
    Effect::new_async(move |_r| {
      Box::pin(async move {
        match inbox.persist_and_queue_job(key, event).await {
          Ok(es_entity::Idempotent::Executed(_)) => Ok(InboxPersistResult::Queued),
          Ok(es_entity::Idempotent::AlreadyApplied) => Ok(InboxPersistResult::AlreadyApplied),
          Err(e) => Err(map_inbox_err(e)),
        }
      })
    })
  }
}
