//! Stage 6, publish: a reviewer publishes a document in review, and the document's
//! `after_update` hook ([`publish_hooks`]) writes [`DocumentPublished`] to the outbox of the
//! tenant's own database **in the update's transaction** (data.orm #7, ADR 0015 D12): the event
//! exists if and only if the publication committed, and a failing hook rolls the update back.

use std::sync::Arc;

use id_effect::{Effect, fail, hlist, succeed};
use id_effect_kit::messaging::{Message, Outbox, Topic};
use id_effect_kit::sql::repository::EntityHooks;
use id_effect_kit::sql::{SqlClientEnv, SqlClientService, SqlTransaction, params};
use id_effect_kit::tenancy::Tenancy;

use crate::App;
use crate::domain::{
  self, DIALECT, DOCUMENT_PUBLISHED_TOPIC, Document, DocumentPublished, DocumentStatus,
  PipelineError,
};

const LABELS_OF: &str = "SELECT l.name FROM labels l \
  JOIN document_labels dl ON dl.label_id = l.id \
  WHERE dl.document_id = $1 AND l.tenant = $2 ORDER BY l.name";

/// The outbox message id of the publication of `document_id` of `tenant` (consumers deduplicate
/// by it).
pub fn published_message_id(tenant: &str, document_id: i64) -> String {
  format!("document-published-{tenant}-{document_id}")
}

/// Stages [`DocumentPublished`] for `document` of `tenant` with `tx`.
fn stage(
  tx: Arc<dyn SqlTransaction>,
  tenant: String,
  document: &Document,
) -> Effect<(), PipelineError> {
  let topic = match Topic::new(DOCUMENT_PUBLISHED_TOPIC) {
    Ok(topic) => topic,
    Err(e) => return fail(PipelineError::Invalid(format!("topic: {e}"))),
  };
  let (id, title) = (document.id, document.title.clone());
  tx.query(LABELS_OF, &params![id, tenant.clone()])
    .map_err(PipelineError::from)
    .flat_map(move |rows| {
      let labels = rows
        .iter()
        .filter_map(|row| row.get::<String>("name").ok())
        .collect();
      let message = Message::encode(DocumentPublished {
        tenant: tenant.clone(),
        document_id: id,
        title,
        labels,
      })
      .with_id(published_message_id(&tenant, id))
      .with_key(tenant);
      Outbox::new()
        .stage(&tx, &topic, message)
        .map(|_| ())
        .map_err(PipelineError::from)
    })
}

/// The hooks of the documents of `tenant`: after every update that leaves a document
/// [`DocumentStatus::Published`], stage its [`DocumentPublished`] in the same transaction.
pub fn publish_hooks(tenant: &str) -> EntityHooks<Document, PipelineError> {
  let tenant = tenant.to_owned();
  EntityHooks::new().after_update(move |ctx, document: &Document| {
    if document.status != DocumentStatus::Published.as_str() {
      return succeed(());
    }
    stage(Arc::clone(ctx.transaction()), tenant.clone(), document)
  })
}

/// Publishes the tenant's document `id`: it must be in [`DocumentStatus::Review`] (and at
/// `version`, when given); the update and its outbox event commit together.
pub fn publish(
  app: &Arc<App>,
  tenancy: &Tenancy,
  id: i64,
  version: Option<i64>,
) -> Effect<Document, PipelineError> {
  let (db, tenant) = (Arc::clone(&app.db), tenancy.tenant().as_str().to_owned());
  let hooked = tenancy
    .repository::<Document>(DIALECT)
    .with_hooks(publish_hooks(&tenant));
  let write = domain::find_document(&db, tenancy, id).flat_map(move |mut document| {
    if document.status != DocumentStatus::Review.as_str() {
      return fail(PipelineError::Conflict(format!(
        "document {id} is {}, not review",
        document.status
      )));
    }
    if version.is_some_and(|v| v != document.version) {
      return fail(PipelineError::Conflict(format!(
        "document {id} was changed by someone else"
      )));
    }
    DocumentStatus::Published
      .as_str()
      .clone_into(&mut document.status);
    let env: SqlClientEnv = hlist![SqlClientService(db)];
    hooked.update::<SqlClientEnv, _>(document).provide_all(env)
  });
  let hub = app.status.clone();
  tenancy.scope(write).map(move |document| {
    hub.announce(&tenant, document.id, DocumentStatus::Published);
    document
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn message_ids_name_tenant_and_document() {
    assert_eq!(published_message_id("acme", 7), "document-published-acme-7");
    assert!(!publish_hooks("acme").is_empty());
  }
}
