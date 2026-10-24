//! Stage 7 (release spec 0.13.0, D1; ai.llm #6): the Pipeline's MCP server, one guarded,
//! Origin-checked Streamable HTTP endpoint per tenant, called by the kit's MCP client.

#[path = "../fixtures/support.rs"]
mod support;

use id_effect_kit::ai::mcp::{HEADER_METHOD, HEADER_PROTOCOL_VERSION, McpClient, PROTOCOL_VERSION};
use id_effect_kit::tenancy::Tenancy;
use id_effect_kit::test::http::{StatusCode, TestClient};
use pipeline::domain::{self, NewDocument};
use pipeline::mcp::{McpSettings, endpoint_path};
use serde_json::json;
use support::{AGENT_ORIGIN, Harness, loopback_http, mcp_request, run, run_typed};

async fn seed(h: &Harness, tenant: &str, titles: &[&str]) -> Vec<i64> {
  let tenancy = Tenancy::new(tenant);
  let mut ids = Vec::new();
  for title in titles {
    let new = NewDocument {
      source_id: h.sources[tenant],
      title: (*title).to_owned(),
      body: format!("{title}."),
    };
    let create = domain::create_document(&h.app.db, &tenancy, new);
    ids.push(run(tenancy.scope(create)).await.expect("document").id);
  }
  ids
}

fn mcp_client(url: String, token: &str) -> McpClient {
  McpClient::http(url, &loopback_http())
    .with_bearer_token(token)
    .expect("a valid token")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_mcp_tools_round_trip() {
  let Some(h) = Harness::start("pipeline_mcp_tools_round_trip", |app| app).await else {
    return;
  };
  let acme_ids = seed(&h, "acme", &["Quarterly invoice", "Supplier contract"]).await;
  seed(&h, "globex", &["Rechnung Q3"]).await;
  let addr = h.serve().await;
  let acme_agent = h.token("bot", Some("acme"), &["agent"]).await;
  let globex_agent = h.token("bot", Some("globex"), &["agent"]).await;
  let acme_url = format!("http://{addr}{}", endpoint_path("acme"));
  let globex_url = format!("http://{addr}{}", endpoint_path("globex"));
  let acme = mcp_client(acme_url.clone(), &acme_agent);

  // The tools, described in the tenant's locale.
  let tools = run(acme.list_tools()).await.expect("tools/list");
  let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
  names.sort_unstable();
  assert_eq!(names, ["get_document", "search_documents"]);
  let search = tools
    .iter()
    .find(|t| t.name == "search_documents")
    .expect("search");
  assert!(
    search
      .description
      .as_deref()
      .is_some_and(|d| d.starts_with("Searches the documents of acme")),
    "{search:?}"
  );
  let german = run(mcp_client(globex_url, &globex_agent).list_tools())
    .await
    .expect("globex tools/list");
  assert!(
    german
      .iter()
      .filter_map(|t| t.description.as_deref())
      .any(|d| d.starts_with("Durchsucht die Dokumente von globex")),
    "{german:?}"
  );

  // search_documents and get_document over the tenant's own documents only.
  let found = run(acme.call_tool("search_documents", json!({ "query": "INVOICE" })))
    .await
    .expect("search_documents");
  assert!(!found.is_error);
  let documents = found.structured_content.expect("structured")["documents"].clone();
  assert_eq!(documents.as_array().map(Vec::len), Some(1));
  assert_eq!(documents[0]["title"], "Quarterly invoice");
  let none = run(acme.call_tool("search_documents", json!({ "query": "Rechnung" })))
    .await
    .expect("search_documents");
  assert_eq!(
    none.structured_content.expect("structured")["documents"],
    json!([])
  );
  let one = run(acme.call_tool("get_document", json!({ "id": acme_ids[1] })))
    .await
    .expect("get_document");
  let detail = one.structured_content.expect("structured");
  assert_eq!(detail["title"], "Supplier contract");
  assert_eq!(detail["source"], "inbox");
  // A missing document is a tool error the agent can read, not a protocol error.
  let missing = run(acme.call_tool("get_document", json!({ "id": 999_999 })))
    .await
    .expect("get_document");
  assert!(missing.is_error);
  assert!(missing.text().contains("not found"), "{}", missing.text());
  // Arguments are decoded through the tool's schema before it runs.
  let malformed = run_typed(acme.call_tool("get_document", json!({ "id": "one" }))).await;
  assert!(
    malformed.is_err() || malformed.as_ref().is_ok_and(|r| r.is_error),
    "{malformed:?}"
  );

  // The endpoint is guarded: another tenant's agent, a non-agent and no token are refused.
  for token in [
    globex_agent.clone(),
    h.token("ed", Some("acme"), &["editor"]).await,
  ] {
    let refused = run_typed(mcp_client(acme_url.clone(), &token).list_tools()).await;
    assert!(refused.is_err(), "{refused:?}");
  }
  let anonymous = run_typed(McpClient::http(acme_url, &loopback_http()).list_tools()).await;
  assert!(anonymous.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_mcp_refuses_foreign_origin() {
  let Some(h) = Harness::start("pipeline_mcp_refuses_foreign_origin", |app| app).await else {
    return;
  };
  let agent = h.token("bot", Some("acme"), &["agent"]).await;
  let mcp = McpSettings {
    allowed_origins: vec![AGENT_ORIGIN.to_owned()],
  };
  let http = TestClient::new(pipeline::router(&h.app, &h.keys, &mcp).expect("router"));
  let path = endpoint_path("acme");
  let call = |origin: Option<&str>, token: Option<&str>| {
    let mut req = http
      .post(&path)
      .header(HEADER_PROTOCOL_VERSION, PROTOCOL_VERSION)
      .header(HEADER_METHOD, "tools/list")
      .json(&mcp_request(1, "tools/list", json!({})));
    if let Some(origin) = origin {
      req = req.header("origin", origin);
    }
    if let Some(token) = token {
      req = req.bearer(token);
    }
    req.send()
  };

  // A browser page on another origin (DNS rebinding) is refused before the server runs.
  let foreign = run(call(Some("https://evil.example"), Some(&agent)))
    .await
    .expect("response");
  foreign.assert_status(StatusCode::FORBIDDEN);
  assert!(!foreign.text().contains("search_documents"));
  // The configured origin and non-browser clients (no Origin) are served.
  for origin in [Some(AGENT_ORIGIN), None] {
    let ok = run(call(origin, Some(&agent))).await.expect("response");
    ok.assert_status(StatusCode::OK);
    assert!(ok.text().contains("search_documents"), "{}", ok.text());
  }
  // Without a token the guard answers first.
  let anonymous = run(call(None, None)).await.expect("response");
  anonymous.assert_status(StatusCode::UNAUTHORIZED);

  // With the default settings no browser origin is admitted at all.
  let closed =
    TestClient::new(pipeline::router(&h.app, &h.keys, &McpSettings::default()).expect("router"));
  let res = run(
    closed
      .post(&path)
      .header(HEADER_PROTOCOL_VERSION, PROTOCOL_VERSION)
      .header(HEADER_METHOD, "tools/list")
      .header("origin", AGENT_ORIGIN)
      .bearer(&agent)
      .json(&mcp_request(1, "tools/list", json!({})))
      .send(),
  )
  .await
  .expect("response");
  res.assert_status(StatusCode::FORBIDDEN);
}
