//! The Pipeline end to end (release spec 0.13.0, D1 stages 1–4, 6 and 7's client half) on
//! PostgreSQL: ingest by RPC and by upload, the stage jobs, live status over the RPC subscription,
//! publication with its outbox event.

#[path = "../fixtures/support.rs"]
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect_kit::ai::mcp::McpClient;
use id_effect_kit::messaging::Message;
use id_effect_kit::sql::params;
use id_effect_kit::tenancy::Tenancy;
use id_effect_kit::test::http::{StatusCode, TestClient};
use pipeline::ai::Enricher;
use pipeline::domain::{
  DOCUMENT_PUBLISHED_TOPIC, DocumentPublished, DocumentStatus, NewDocument, PipelineError,
};
use pipeline::mcp::McpSettings;
use pipeline::rpc::IntakeError;
use pipeline::stages;
use support::{Harness, client, fixture_mcp_server, run, run_typed, well_behaved};

const OUTBOX_ROWS: &str = "SELECT count(*) FROM id_effect_outbox WHERE topic = $1";

/// An enricher speaking to the fixture MCP tool server over an in-memory pipe.
async fn fixture_enricher() -> Enricher {
  let (client_io, server_io) = tokio::io::duplex(64 * 1024);
  let (server_read, server_write) = tokio::io::split(server_io);
  let serve = fixture_mcp_server().serve_io(server_read, server_write);
  tokio::spawn(async move {
    let _ = run(serve).await;
  });
  let (client_read, client_write) = tokio::io::split(client_io);
  let client = run(McpClient::over_io(client_read, client_write))
    .await
    .expect("the MCP client connects");
  Enricher::new(client, "lookup_organization")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_ingest_to_publish_end_to_end() {
  let enricher = fixture_enricher().await;
  let Some(h) = Harness::start("pipeline_ingest_to_publish_end_to_end", |app| {
    app.with_enricher(enricher)
  })
  .await
  else {
    return;
  };
  let addr = h.serve().await;
  let editor = h.token("ed", Some("acme"), &["editor"]).await;
  let reviewer = h.token("rev", Some("acme"), &["reviewer"]).await;
  let title = "Invoice 2026-001";
  h.scripts
    .add(title, well_behaved(title, "en", &["invoice"]));

  // Stage 1 by RPC mutation.
  let ingested = run(client(addr, &editor).ingest(NewDocument {
    source_id: h.sources["acme"],
    title: title.to_owned(),
    body: "Acme Corp bills 12.50 EUR for widgets.".to_owned(),
  }))
  .await
  .expect("ingest");
  assert_eq!(ingested.status, "ingested");
  let id = ingested.id;

  // Live status: the subscription's first item is the current status.
  let seen = Arc::new(Mutex::new(Vec::<String>::new()));
  let watching = {
    let (seen, reviewer_client) = (Arc::clone(&seen), client(addr, &reviewer));
    tokio::spawn(async move {
      let watch = reviewer_client.status(id, None).flat_map(move |items| {
        items
          .map(move |item| {
            seen.lock().expect("seen").push(item.value.status.clone());
            item
          })
          .run_collect()
      });
      run(watch).await
    })
  };
  let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
  while seen.lock().expect("seen").is_empty() {
    if watching.is_finished() {
      panic!("the subscription ended: {:?}", watching.await);
    }
    assert!(tokio::time::Instant::now() < deadline, "no first status");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }

  // Stages 2–4 as jobs: extraction (enriched over MCP), classification, embedding.
  h.drain().await;
  let detail = run(client(addr, &reviewer).document(id))
    .await
    .expect("document");
  assert_eq!(detail.status, "review");
  assert_eq!(detail.labels, ["invoice"]);
  assert_eq!(detail.source, "inbox");
  let extraction = detail.extraction.expect("an extraction");
  assert_eq!(extraction.language, "en");
  assert_eq!(extraction.organization.as_deref(), Some("Acme Corp"));
  let enrichment = extraction.enrichment.expect("the MCP enrichment");
  assert!(enrichment.contains("BE-0123.456.789"), "{enrichment}");

  // Editors cannot publish; reviewers can.
  let denied = run(client(addr, &editor).publish(id)).await;
  assert!(denied.is_err(), "an editor published");
  let published = run(client(addr, &reviewer).publish(id))
    .await
    .expect("publish");
  assert_eq!(published.status, "published");

  let statuses = watching
    .await
    .expect("the watch task")
    .expect("the subscription");
  assert_eq!(statuses.len(), 5);
  assert_eq!(
    *seen.lock().expect("seen"),
    ["ingested", "extracted", "classified", "review", "published"]
  );
  assert_eq!(
    Harness::count(&h.main, OUTBOX_ROWS, params![DOCUMENT_PUBLISHED_TOPIC]).await,
    1
  );

  // Stage 1 by upload: the raw text is kept in object storage and ingested.
  let upload_title = "Uploaded contract";
  h.scripts.add(
    upload_title,
    well_behaved(upload_title, "en", &["contract"]),
  );
  let mcp = McpSettings::default();
  let http = TestClient::new(pipeline::router(&h.app, &h.keys, &mcp).expect("router"));
  let res = run(
    http
      .post(&format!(
        "/documents/upload?source_id={}&title=Uploaded%20contract",
        h.sources["acme"]
      ))
      .bearer(&editor)
      .body("This contract binds Acme Corp.", "text/plain")
      .send(),
  )
  .await
  .expect("upload");
  res.assert_status(StatusCode::CREATED);
  let uploaded: serde_json::Value = res.json().expect("json");
  assert_eq!(uploaded["status"], "ingested");
  let anonymous = run(
    http
      .post("/documents/upload?source_id=1&title=x")
      .body("x", "text/plain")
      .send(),
  )
  .await
  .expect("anonymous upload");
  anonymous.assert_status(StatusCode::UNAUTHORIZED);
  h.drain().await;
  let uploaded_id = uploaded["id"].as_i64().expect("an id");
  assert_eq!(h.status("acme", uploaded_id).await, "review");
  let health = run(http.get("/health").send()).await.expect("health");
  health.assert_status(StatusCode::OK);
}

#[test]
fn pipeline_routes_declare_their_access() {
  // No database is reached to build the router: the clients connect on first use.
  let tenants = support::tenants("postgres://unused/globex");
  let control: Arc<dyn id_effect_kit::sql::SqlClient> =
    Arc::new(id_effect_kit::sql::TestSqlClient::new());
  let scripted = id_effect_kit::ai::testing::ScriptedModel::new();
  let app = Arc::new(pipeline::App::new(
    tenants,
    id_effect_kit::tenancy::TenantRouter::from_map(Vec::<(
      &str,
      id_effect_kit::sql::TenantClientFactory,
    )>::new()),
    control,
    scripted.service(),
    scripted.embedder_service(),
  ));
  let routes = pipeline::routes(&app, &McpSettings::default()).expect("routes");
  let report = routes.access_report();
  // `/rpc` is public at the route level only: every procedure carries its own guard.
  assert!(
    report
      .public
      .iter()
      .all(|route| route.ends_with(" /health") || route.contains(" /rpc")),
    "{:?}",
    report.public
  );
  assert!(
    report
      .public
      .iter()
      .any(|route| route.ends_with(" /health"))
  );
  for path in ["/documents/upload", "/mcp/acme", "/mcp/globex"] {
    assert!(
      report
        .guarded
        .iter()
        .any(|(route, policies)| route.ends_with(path) && !policies.is_empty()),
      "{path} in {:?}",
      report.guarded
    );
  }
  let contract = pipeline::rpc::IntakeService::contract();
  assert_eq!(contract.procedures.len(), 5);
  for procedure in &contract.procedures {
    assert!(
      matches!(
        procedure.access,
        id_effect_kit::rpc::contract::ProcedureAccess::Guarded { .. }
      ),
      "{} is not guarded",
      procedure.name
    );
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_publish_hook_writes_outbox() {
  let Some(h) = Harness::start("pipeline_publish_hook_writes_outbox", |app| app).await else {
    return;
  };
  let globex = Tenancy::new("globex");
  for title in ["Rechnung 1", "Rechnung 2"] {
    h.scripts
      .add(title, well_behaved(title, "de", &["rechnung"]));
    let new = NewDocument {
      source_id: h.sources["globex"],
      title: title.to_owned(),
      body: format!("{title}: Globex GmbH stellt 40 EUR in Rechnung."),
    };
    run(stages::ingest(&h.app, &globex, new))
      .await
      .expect("ingest");
  }
  h.drain().await;

  // The hook writes the event to globex's own database, in the update's transaction.
  let published = run(stages::publish(&h.app, &globex, 1, None))
    .await
    .expect("publish");
  assert_eq!(published.status, DocumentStatus::Published.as_str());
  let rows = run(h.globex.query(
    "SELECT message_id, message_key, payload FROM id_effect_outbox WHERE topic = $1",
    &params![DOCUMENT_PUBLISHED_TOPIC],
  ))
  .await
  .expect("outbox");
  assert_eq!(rows.len(), 1);
  assert_eq!(
    rows[0].get::<String>("message_id").expect("id"),
    "document-published-globex-1"
  );
  assert_eq!(
    rows[0]
      .get::<Option<String>>("message_key")
      .expect("key")
      .as_deref(),
    Some("globex")
  );
  let event: DocumentPublished = Message::new(rows[0].get::<Vec<u8>>("payload").expect("payload"))
    .decode()
    .expect("a DocumentPublished");
  assert_eq!(event.tenant, "globex");
  assert_eq!(event.document_id, 1);
  assert_eq!(event.labels, ["rechnung"]);
  assert_eq!(
    Harness::count(&h.main, OUTBOX_ROWS, params![DOCUMENT_PUBLISHED_TOPIC]).await,
    0,
    "nothing in the main database"
  );

  // Publishing twice is a conflict and writes nothing more.
  let again = run_typed(stages::publish(&h.app, &globex, 1, None)).await;
  assert!(
    matches!(again, Err(PipelineError::Conflict(_))),
    "{again:?}"
  );
  // A stale version is a conflict too.
  let stale = run_typed(stages::publish(&h.app, &globex, 2, Some(1))).await;
  assert!(
    matches!(stale, Err(PipelineError::Conflict(_))),
    "{stale:?}"
  );

  // The hook and the update commit together: without an outbox the publication rolls back.
  run(
    h.globex
      .execute("ALTER TABLE id_effect_outbox RENAME TO outbox_away", &[]),
  )
  .await
  .expect("rename");
  let failed = run_typed(stages::publish(&h.app, &globex, 2, None)).await;
  assert!(failed.is_err(), "published without an outbox");
  assert_eq!(h.status("globex", 2).await, "review");
  run(
    h.globex
      .execute("ALTER TABLE outbox_away RENAME TO id_effect_outbox", &[]),
  )
  .await
  .expect("rename back");
  run(stages::publish(&h.app, &globex, 2, None))
    .await
    .expect("publish");
  assert_eq!(
    Harness::count(&h.globex, OUTBOX_ROWS, params![DOCUMENT_PUBLISHED_TOPIC]).await,
    2
  );

  // The RPC error of a missing document is NotFound, not a server fault.
  assert!(matches!(
    IntakeError::from(PipelineError::NotFound("document 9".into())),
    IntakeError::NotFound { .. }
  ));
}
