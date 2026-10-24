//! Live status: every stage announces a document's new status on the [`StatusHub`], and
//! [`watch`] turns the announcements for one document into the stream the `Intake.status`
//! subscription serves (its current status first, then each change, ending at a final status).
//!
//! The hub is in-process (a bounded broadcast channel): a slow subscriber that falls behind
//! skips ahead to the document's current status read from the database, so it never shows a
//! stale state for long and never blocks a stage.

use std::sync::Arc;

use id_effect::{Ctx, DynEnv, Effect, Stream};
use id_effect_kit::tenancy::Tenancy;
use tokio::sync::{Mutex, broadcast};

use crate::domain::{self, DocumentStatus, PipelineError, StatusUpdate};

/// How many announcements the hub buffers per subscriber before it falls behind.
pub const HUB_CAPACITY: usize = 256;

/// One announcement: tenant `tenant`'s document `document_id` is now `status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEvent {
  /// The tenant.
  pub tenant: String,
  /// The document.
  pub document_id: i64,
  /// Its new status.
  pub status: DocumentStatus,
}

/// The in-process announcement channel of status changes. Clones share it.
#[derive(Clone, Debug)]
pub struct StatusHub {
  tx: broadcast::Sender<StatusEvent>,
}

impl Default for StatusHub {
  fn default() -> Self {
    Self::new()
  }
}

impl StatusHub {
  /// A hub buffering [`HUB_CAPACITY`] announcements per subscriber.
  pub fn new() -> Self {
    Self {
      tx: broadcast::channel(HUB_CAPACITY).0,
    }
  }

  /// Announces that `document_id` of `tenant` is now `status` (nobody listening is fine).
  pub fn announce(&self, tenant: &str, document_id: i64, status: DocumentStatus) {
    let _ = self.tx.send(StatusEvent {
      tenant: tenant.to_owned(),
      document_id,
      status,
    });
  }

  /// A new subscriber, seeing every announcement from now on.
  pub fn subscribe(&self) -> broadcast::Receiver<StatusEvent> {
    self.tx.subscribe()
  }
}

/// Where one watch is.
struct Watch {
  rx: broadcast::Receiver<StatusEvent>,
  last: Option<DocumentStatus>,
  done: bool,
}

/// The status of `document_id` of `tenancy` now, then every change until it is final. Fails
/// with [`PipelineError::NotFound`] before the first item when the tenant has no such document.
pub fn watch<E>(
  db: &Arc<dyn id_effect_kit::sql::SqlClient>,
  hub: &StatusHub,
  tenancy: &Tenancy,
  document_id: i64,
) -> Effect<Stream<StatusUpdate, E, DynEnv>, E, DynEnv>
where
  E: From<PipelineError> + Send + 'static,
{
  // Subscribe before reading, so a change between the read and the first pull is not lost.
  let rx = hub.subscribe();
  let (db, tenancy) = (Arc::clone(db), tenancy.clone());
  let read = tenancy.scope(domain::find_document(&db, &tenancy, document_id));
  read.map_err(E::from).any_env().map(move |document| {
    let first = DocumentStatus::parse(&document.status);
    let state = Arc::new(Mutex::new(Watch {
      rx,
      last: None,
      done: false,
    }));
    let tenant = tenancy.tenant().as_str().to_owned();
    let pending = Arc::new(std::sync::Mutex::new(first));
    Stream::from_pull(move |_| {
      let (state, pending, tenant) = (Arc::clone(&state), Arc::clone(&pending), tenant.clone());
      let (db, tenancy) = (Arc::clone(&db), tenancy.clone());
      Effect::from_ctx(move |ctx: Ctx<DynEnv>| async move {
        let mut watch = state.lock().await;
        if watch.done {
          return Ok(None);
        }
        let mut next = pending
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .take();
        loop {
          if let Some(status) = next.filter(|s| watch.last != Some(*s)) {
            watch.last = Some(status);
            watch.done = status.is_final();
            return Ok(Some(vec![StatusUpdate {
              document_id,
              status: status.as_str().to_owned(),
            }]));
          }
          next = match watch.rx.recv().await {
            Ok(e) if e.tenant == tenant && e.document_id == document_id => Some(e.status),
            Ok(_) => None,
            Err(broadcast::error::RecvError::Lagged(_)) => {
              // Fell behind: the database has the current status.
              let read = tenancy.scope(domain::find_document(&db, &tenancy, document_id));
              match ctx.bind(read).await {
                Ok(d) => DocumentStatus::parse(&d.status),
                Err(e) => return Err(E::from(e)),
              }
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(None),
          };
        }
      })
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_hub_broadcasts_to_every_subscriber() {
    let hub = StatusHub::default();
    let mut a = hub.subscribe();
    let mut b = hub.clone().subscribe();
    hub.announce("acme", 7, DocumentStatus::Review);
    let expected = StatusEvent {
      tenant: "acme".into(),
      document_id: 7,
      status: DocumentStatus::Review,
    };
    assert_eq!(a.try_recv().unwrap(), expected);
    assert_eq!(b.try_recv().unwrap(), expected);
  }
}
