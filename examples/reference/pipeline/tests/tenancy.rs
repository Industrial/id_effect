//! Tenancy and the data model on PostgreSQL (release spec 0.13.0, D1 and D10): isolation across
//! relations, per-tenant database routing, eager loading without N+1 and keyset pages that stay
//! stable while documents arrive.

#[path = "../fixtures/support.rs"]
mod support;

use std::collections::BTreeSet;
use std::sync::Arc;

use id_effect_kit::sql::{Entity, SqlClient, params};
use id_effect_kit::tenancy::Tenancy;
use pipeline::domain::{self, Document, NewDocument, PageRequest, PipelineError};
use pipeline::rpc::IntakeError;
use pipeline::stages;
use support::{Harness, Probe, client, run, run_typed, well_behaved};

const DOCUMENTS_OF: &str = "SELECT count(*) FROM documents WHERE tenant = $1";

fn new_document(source_id: i64, title: &str) -> NewDocument {
  NewDocument {
    source_id,
    title: title.to_owned(),
    body: format!("The text of {title}."),
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_tenant_isolation_across_relations() {
  let Some(h) = Harness::start("pipeline_tenant_isolation_across_relations", |app| app).await
  else {
    return;
  };
  let acme = Tenancy::new("acme");
  let title = "Acme invoice";
  h.scripts
    .add(title, well_behaved(title, "en", &["invoice"]));
  let document = run(stages::ingest(
    &h.app,
    &acme,
    new_document(h.sources["acme"], title),
  ))
  .await
  .expect("ingest");
  h.drain().await;

  // Stray rows of another tenant in acme's (shared) database, linked to acme's document: a
  // label through the join table, an extraction through the foreign key, a document.
  let stray_label = run(h.main.query(
    "INSERT INTO labels (tenant, name, description) VALUES ('globex', 'secret', 'x') RETURNING id",
    &[],
  ))
  .await
  .expect("stray label")[0]
    .get::<i64>("id")
    .expect("id");
  run(h.main.execute(
    "INSERT INTO document_labels (document_id, label_id) VALUES ($1, $2)",
    &params![document.id, stray_label],
  ))
  .await
  .expect("stray link");
  run(h.main.execute(
    "INSERT INTO extractions (tenant, document_id, title, summary, language, model) \
     VALUES ('globex', $1, 'leaked', 'leaked', 'de', 'm')",
    &params![document.id],
  ))
  .await
  .expect("stray extraction");
  let stray_document = run(h.main.query(
    "INSERT INTO documents (tenant, source_id, title, body, status) \
     VALUES ('globex', $1, 'Globex secret', 'secret', 'review') RETURNING id",
    &params![h.sources["acme"]],
  ))
  .await
  .expect("stray document")[0]
    .get::<i64>("id")
    .expect("id");

  // Eager loading re-applies the child's tenant: none of the stray rows loads.
  let detail = run(acme.scope(domain::document_detail(&h.app.db, &acme, document.id)))
    .await
    .expect("detail");
  assert_eq!(detail.labels, ["invoice"]);
  let extraction = detail.extraction.expect("acme's extraction");
  assert_ne!(extraction.title, "leaked");
  // Another tenant's document is not found, not listed, not searchable.
  let found = run_typed(acme.scope(domain::find_document(&h.app.db, &acme, stray_document))).await;
  assert!(
    matches!(found, Err(PipelineError::NotFound(_))),
    "{found:?}"
  );
  let page = run(acme.scope(domain::list_documents(
    &h.app.db,
    &acme,
    &PageRequest::default(),
  )))
  .await
  .expect("page");
  assert_eq!(
    page.items.iter().map(|d| d.id).collect::<Vec<_>>(),
    [document.id]
  );
  let search = run(acme.scope(domain::search_documents(&h.app.db, &acme, "secret", 10)))
    .await
    .expect("search");
  assert!(search.is_empty());

  // Labels are only attached from the document's own tenant.
  let attached = run(acme.scope(domain::attach_labels(
    &h.app.db,
    &acme,
    document.id,
    vec!["secret".to_owned(), "contract".to_owned()],
  )))
  .await
  .expect("attach");
  assert_eq!(attached, 1, "only acme's `contract`");

  // Over RPC: a globex reviewer cannot read acme's document id (globex has none).
  let addr = h.serve().await;
  let globex_reviewer = h.token("g", Some("globex"), &["reviewer"]).await;
  let across = run_typed(client(addr, &globex_reviewer).document(document.id)).await;
  let err = across.expect_err("read across tenants");
  assert!(
    matches!(err.typed(), Some(IntakeError::NotFound { .. })),
    "{err:?}"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_tenant_db_routing() {
  let Some(h) = Harness::start("pipeline_tenant_db_routing", |app| app).await else {
    return;
  };
  let addr = h.serve().await;
  let acme_editor = h.token("a", Some("acme"), &["editor"]).await;
  let globex_editor = h.token("g", Some("globex"), &["editor"]).await;
  run(client(addr, &acme_editor).ingest(new_document(h.sources["acme"], "Acme memo")))
    .await
    .expect("acme ingest");
  run(client(addr, &globex_editor).ingest(new_document(h.sources["globex"], "Globex Notiz")))
    .await
    .expect("globex ingest");

  // acme's rows are in the main database, globex's in its own.
  let count = |db: Arc<dyn SqlClient>, tenant: &'static str| async move {
    Harness::count(&db, DOCUMENTS_OF, params![tenant]).await
  };
  assert_eq!(count(Arc::clone(&h.main), "acme").await, 1);
  assert_eq!(count(Arc::clone(&h.main), "globex").await, 0);
  assert_eq!(count(Arc::clone(&h.globex), "globex").await, 1);
  assert_eq!(count(Arc::clone(&h.globex), "acme").await, 0);
  let mut open = h.app.router.open_tenants();
  open.sort();
  assert_eq!(open, ["acme", "globex"]);
  assert!(h.app.router.pool_count() <= h.app.router.max_pools());

  // A tenant without a database has no connection: refused, nothing written anywhere.
  let initech = h.token("i", Some("initech"), &["editor"]).await;
  let refused = run_typed(client(addr, &initech).ingest(new_document(1, "Initech memo"))).await;
  let err = refused.expect_err("an unknown tenant ingested");
  assert!(
    matches!(err.typed(), Some(IntakeError::Forbidden { .. })),
    "{err:?}"
  );
  // A principal without a tenant claim is stopped by the guard (403).
  let no_tenant = h.token("n", None, &["editor"]).await;
  assert!(
    run_typed(client(addr, &no_tenant).documents(PageRequest::default()))
      .await
      .is_err()
  );
  // Outside a tenant the router has no connection at all.
  let outside = run_typed(h.app.db.query("SELECT 1", &[])).await;
  assert!(outside.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_eager_loading_no_n_plus_one() {
  let Some(h) = Harness::start("pipeline_eager_loading_no_n_plus_one", |app| app).await else {
    return;
  };
  let acme = Tenancy::new("acme");
  let mut ids = Vec::new();
  for n in 0..6 {
    let title = format!("Contract {n}");
    let label = if n % 2 == 0 { "contract" } else { "invoice" };
    h.scripts.add(&title, well_behaved(&title, "en", &[label]));
    let document = run(stages::ingest(
      &h.app,
      &acme,
      new_document(h.sources["acme"], &title),
    ))
    .await
    .expect("ingest");
    ids.push(document.id);
  }
  h.drain().await;

  // One query for the documents and one per relation (source, labels, extractions): four,
  // for one document as for six.
  for batch in [vec![ids[0]], ids.clone()] {
    let probe = Probe::new(Arc::clone(&h.app.db));
    let db: Arc<dyn SqlClient> = Arc::new(probe.clone());
    let loaded = run(acme.scope(domain::documents_with_relations(&db, &acme, batch.clone())))
      .await
      .expect("eager load");
    assert_eq!(loaded.len(), batch.len());
    assert_eq!(probe.queries().len(), 4, "{:#?}", probe.queries());
    for document in &loaded {
      let detail = domain::detail_of(document);
      assert_eq!(detail.source, "inbox");
      assert_eq!(detail.labels.len(), 1);
      assert!(detail.extraction.is_some());
    }
  }
  assert_eq!(Document::RELATIONS.len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_keyset_pagination_stable_under_inserts() {
  let Some(h) = Harness::start("pipeline_keyset_pagination_stable_under_inserts", |app| app).await
  else {
    return;
  };
  let acme = Tenancy::new("acme");
  let create = |title: String| {
    let (db, acme, source) = (Arc::clone(&h.app.db), acme.clone(), h.sources["acme"]);
    async move {
      run(acme.scope(domain::create_document(
        &db,
        &acme,
        new_document(source, &title),
      )))
      .await
      .expect("create")
      .id
    }
  };
  for n in 0..5 {
    create(format!("Early {n}")).await;
  }
  let page = |cursor: Option<String>| {
    let (db, acme) = (Arc::clone(&h.app.db), acme.clone());
    async move {
      let request = PageRequest {
        cursor,
        size: Some(2),
      };
      run(acme.scope(domain::list_documents(&db, &acme, &request)))
        .await
        .expect("page")
    }
  };

  // Page through while documents keep arriving: every document once, in order.
  let mut seen = Vec::new();
  let mut cursor = None;
  let mut pages = 0;
  loop {
    let current = page(cursor).await;
    seen.extend(current.items.iter().map(|d| d.id));
    pages += 1;
    if pages <= 3 {
      create(format!("Late {pages}")).await;
    }
    match current.next {
      Some(next) => cursor = Some(next),
      None => break,
    }
  }
  assert_eq!(seen.len(), 8, "{seen:?}");
  let unique: BTreeSet<i64> = seen.iter().copied().collect();
  assert_eq!(unique.len(), 8);
  assert!(seen.windows(2).all(|w| w[0] < w[1]), "{seen:?}");

  // A cursor that is not one, or a page size outside the limits, is refused.
  let bad = run_typed(acme.scope(domain::list_documents(
    &h.app.db,
    &acme,
    &PageRequest {
      cursor: Some("not-a-cursor".to_owned()),
      size: None,
    },
  )))
  .await;
  assert!(matches!(bad, Err(PipelineError::Invalid(_))), "{bad:?}");
  let zero = run_typed(acme.scope(domain::list_documents(
    &h.app.db,
    &acme,
    &PageRequest {
      cursor: None,
      size: Some(0),
    },
  )))
  .await;
  assert!(matches!(zero, Err(PipelineError::Invalid(_))));
  let capped = page_of_size(&h, &acme, 10_000).await;
  assert_eq!(capped.items.len(), 8);
}

async fn page_of_size(h: &Harness, acme: &Tenancy, size: i64) -> domain::DocumentPage {
  let request = PageRequest {
    cursor: None,
    size: Some(size),
  };
  run(acme.scope(domain::list_documents(&h.app.db, acme, &request)))
    .await
    .expect("page")
}
