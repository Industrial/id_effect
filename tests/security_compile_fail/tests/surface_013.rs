//! The breadth-battery security proof of 0.13 (r13-e-fuzz-security; ADR 0013 applied to the
//! breadth batteries, release spec section "Security").
//!
//! - `every_security_table_test_exists`: every id the 0.13 release spec's Security table names in
//!   its enforcement column is a test function, a compile-fail case under `ui/`, or (after the
//!   word "fuzz") a cargo-fuzz target with seeds; a renamed or deleted test fails here, as the
//!   0.12 `crates/id_effect_rpc/tests/security_surface.rs` does for the call surface.
//! - `fuzz_targets_cover_the_breadth_decoders`: the six 0.13 decoders have targets, `[[bin]]`
//!   entries and seed corpora, and the fuzz crate has at least 22 targets.
//! - `cross_crate_cases_against_one_app`: one application merging the admin site and the MCP
//!   Streamable HTTP endpoint into one typed router behind an authentication middleware and the
//!   i18n locale layer, and the defaults that cross crates: no public route, the guard runs
//!   before either battery (401 / 403), the MCP Origin allowlist and message cap, and a bounded
//!   `Accept-Language` negotiated only to an allowlisted locale.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The workspace root.
fn root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &Path) -> String {
  std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `.rs` file under `dir` (recursively), skipping build output.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    let name = entry.file_name();
    if path.is_dir() {
      if name != "target" && name != ".git" {
        rust_files(&path, out);
      }
    } else if path.extension().is_some_and(|e| e == "rs") {
      out.push(path);
    }
  }
}

/// The names `text` declares with `fn <name>` followed by `(` or `<`.
fn declared_fns(text: &str) -> impl Iterator<Item = &str> {
  text.match_indices("fn ").filter_map(move |(at, _)| {
    let before = text[..at].chars().next_back();
    if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
      return None;
    }
    let rest = &text[at + 3..];
    let len = rest
      .find(|c: char| !(c.is_alphanumeric() || c == '_'))
      .unwrap_or(rest.len());
    let next = rest[len..].chars().next();
    (len > 0 && matches!(next, Some('(' | '<'))).then(|| &rest[..len])
  })
}

/// Whether `text` declares `fn <name>` (followed by `(` or `<`).
fn declares_fn(text: &str, name: &str) -> bool {
  declared_fns(text).any(|declared| declared == name)
}

// ---------------------------------------------------------------------------------------------
// The Security table
// ---------------------------------------------------------------------------------------------

/// Ids of the Security table whose leaf has not landed yet, with that leaf; the exit gate
/// (`r13-w8-kpis`) runs with this list empty.
const PENDING: &[(&str, &str)] = &[];

/// How the Security table enforces one id.
#[derive(Debug, PartialEq, Eq)]
enum Check {
  /// A test function or a compile-fail case of that name.
  Test(String),
  /// A cargo-fuzz target of that name (the id follows the word "fuzz").
  Fuzz(String),
}

impl Check {
  fn id(&self) -> &str {
    match self {
      Self::Test(id) | Self::Fuzz(id) => id,
    }
  }
}

/// The backticked ids of the Enforcement-point column of the Security table: snake_case with at
/// least one `_`, not an opt-out name; an id right after the word "fuzz" is a fuzz target.
fn security_table_checks(spec: &str) -> Vec<Check> {
  let section = spec
    .split("\n## ")
    .find(|s| s.starts_with("Security"))
    .expect("the release spec has a Security section");
  let mut checks = Vec::new();
  for row in section.lines().filter(|l| l.starts_with('|')).skip(2) {
    let cells: Vec<&str> = row.split('|').collect();
    let Some(enforcement) = cells.get(4) else {
      continue;
    };
    let mut fuzz = false;
    for (i, part) in enforcement.split('`').enumerate() {
      if i % 2 == 0 {
        // Prose between ids: "fuzz `a`, `b`" makes every following id in the cell a target.
        if part.contains("fuzz") {
          fuzz = true;
        } else if part.contains(';') {
          fuzz = false;
        }
        continue;
      }
      let id = part.trim();
      let is_id = id.contains('_')
        && id
          .bytes()
          .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !id.starts_with("dangerous_")
        && !id.starts_with("insecure_");
      if is_id {
        checks.push(if fuzz {
          Check::Fuzz(id.to_owned())
        } else {
          Check::Test(id.to_owned())
        });
      }
    }
  }
  checks
}

/// The number of committed `seed-*` files of fuzz target `target`.
fn seeds(target: &str) -> usize {
  std::fs::read_dir(root().join(format!("fuzz/corpus/{target}")))
    .map(|d| {
      d.flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("seed-"))
        .count()
    })
    .unwrap_or(0)
}

/// Whether fuzz target `target` exists with its `[[bin]]` and at least three seeds.
fn fuzz_target_exists(manifest: &str, target: &str) -> bool {
  root()
    .join(format!("fuzz/fuzz_targets/{target}.rs"))
    .is_file()
    && manifest.contains(&format!("name = \"{target}\""))
    && seeds(target) >= 3
}

#[test]
fn every_security_table_test_exists() {
  let root = root();
  let spec = read(&root.join(".maestro/specs/releases/0.13.0.md"));
  let checks = security_table_checks(&spec);
  assert!(
    checks.len() >= 50,
    "only {} ids in the Security table: {checks:?}",
    checks.len()
  );
  let manifest = read(&root.join("fuzz/Cargo.toml"));
  let mut files = Vec::new();
  for dir in ["crates", "tests", "examples"] {
    rust_files(&root.join(dir), &mut files);
  }
  // Every function name declared in the tree, read once (the tree is large).
  let declared: HashSet<String> = files
    .iter()
    .flat_map(|p| {
      let text = std::fs::read_to_string(p).unwrap_or_default();
      declared_fns(&text).map(str::to_owned).collect::<Vec<_>>()
    })
    .collect();
  let mut missing = Vec::new();
  for check in &checks {
    let found = match check {
      Check::Test(id) => {
        root
          .join(format!("tests/security_compile_fail/ui/{id}.rs"))
          .is_file()
          || declared.contains(id)
      }
      Check::Fuzz(id) => fuzz_target_exists(&manifest, id),
    };
    if !found && !PENDING.iter().any(|(p, _)| *p == check.id()) {
      missing.push(check.id().to_owned());
    }
  }
  assert!(
    missing.is_empty(),
    "Security-table checks that do not exist in the workspace: {missing:?}"
  );
  for (id, leaf) in PENDING {
    assert!(
      checks.iter().any(|c| c.id() == *id),
      "PENDING `{id}` is not in the Security table"
    );
    eprintln!("surface_013: `{id}` is pending on {leaf}");
  }
}

#[test]
fn the_table_parser_reads_the_enforcement_column_only() {
  let spec = "x\n## Security — t\n\n| Threat | Default | Rule | Enforcement point | Leaf |\n\
              |---|---|---|---|---|\n\
              | a | `not_this_one` | R1 | compile-fail `r1_case`; `some_test`; posture probe \"x\"; \
              fuzz `fuzz_one`, `fuzz_two` | `leaf_slug` |\n\
              | b | d | R4 | `dangerous_any_origin` recorded; `Origin`; `after_fuzz_row` | l |\n\
              \n## Next\n| x | y | z | `other_test` | w |\n";
  assert_eq!(
    security_table_checks(spec),
    [
      Check::Test("r1_case".into()),
      Check::Test("some_test".into()),
      Check::Fuzz("fuzz_one".into()),
      Check::Fuzz("fuzz_two".into()),
      Check::Test("after_fuzz_row".into()),
    ]
  );
  assert!(declares_fn("async fn some_test() {}", "some_test"));
  assert!(declares_fn("fn g<T>() {}", "g"));
  assert!(!declares_fn("fn some_test_longer() {}", "some_test"));
}

// ---------------------------------------------------------------------------------------------
// Fuzz targets
// ---------------------------------------------------------------------------------------------

/// The 0.13 decoder targets (release spec, leaf r13-e-fuzz-security).
const BREADTH_FUZZ_TARGETS: &[&str] = &[
  "i18n_accept_language",
  "sql_page_cursor",
  "mcp_jsonrpc_message",
  "ai_sse_event",
  "ai_tool_arguments",
  "admin_form_body",
];

#[test]
fn fuzz_targets_cover_the_breadth_decoders() {
  let fuzz = root().join("fuzz");
  let manifest = read(&fuzz.join("Cargo.toml"));
  let targets = std::fs::read_dir(fuzz.join("fuzz_targets"))
    .unwrap()
    .flatten()
    .filter(|e| e.path().extension().is_some_and(|e| e == "rs"))
    .count();
  assert!(
    targets >= 22,
    "{targets} fuzz targets, 0.13 needs at least 22"
  );
  for target in BREADTH_FUZZ_TARGETS {
    assert!(
      fuzz_target_exists(&manifest, target),
      "fuzz target {target} is missing, has no [[bin]] or fewer than 3 seeds ({})",
      seeds(target)
    );
  }
  let doc = read(&root().join("docs/security/fuzzing.md"));
  for target in BREADTH_FUZZ_TARGETS {
    assert!(
      doc.contains(&format!("| `{target}` |")),
      "docs/security/fuzzing.md does not list {target}"
    );
  }
}

// ---------------------------------------------------------------------------------------------
// One application, the cross-crate defaults
// ---------------------------------------------------------------------------------------------

mod one_app {
  use std::sync::Arc;

  use axum::body::Body;
  use axum::extract::Request;
  use axum::http::{StatusCode, header};
  use axum::middleware::Next;
  use axum::response::Response;
  use id_effect_admin::{AdminDatabase, AdminSite, EntityAdmin};
  use id_effect_ai::mcp::{McpEndpoint, McpServer, PROTOCOL_VERSION};
  use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Guard, require_role};
  use id_effect_i18n::axum::LocaleLayer;
  use id_effect_i18n::negotiate::MAX_ACCEPT_LANGUAGE_BYTES;
  use id_effect_i18n::{Catalog, Locale};
  use id_effect_kit::axum::realtime::origin::OriginPolicy;
  use id_effect_kit::axum::route::{AccessReport, Router};
  use id_effect_sql::{Dialect, Entity, FromRow, TestSqlClient};
  use serde_json::json;
  use tower::ServiceExt as _;

  const APP_ORIGIN: &str = "https://app.example";

  /// A tenant-scoped entity, so the admin mounts its full set of routes.
  #[derive(Clone, Debug, PartialEq, FromRow, Entity)]
  #[entity(table = "documents", tenant = "org_id")]
  struct Document {
    #[entity(id, generated)]
    id: i64,
    #[entity(editable)]
    title: String,
  }

  /// The test's stand-in for an authentication layer: `Authorization: Bearer <user>:<role>`
  /// becomes the request's principal.
  async fn authenticate(mut req: Request, next: Next) -> Response {
    let principal = req
      .headers()
      .get(header::AUTHORIZATION)
      .and_then(|v| v.to_str().ok())
      .and_then(|v| v.strip_prefix("Bearer "))
      .and_then(|v| v.split_once(':'))
      .map(|(user, role)| BasicPrincipal::new(user).with_role(role));
    if let Some(principal) = principal {
      req
        .extensions_mut()
        .insert(CurrentPrincipal::new(principal));
    }
    next.run(req).await
  }

  fn catalog() -> Catalog {
    let en = Locale::parse("en").unwrap();
    let de = Locale::parse("de").unwrap();
    Catalog::builder(en.clone())
      .embedded(en, "en.ftl", "hello = Hello")
      .embedded(de, "de.ftl", "hello = Hallo")
      .build()
      .unwrap()
  }

  /// The admin site and the MCP endpoint in one typed router, each behind its own guard: the
  /// router's access report and the application served from it.
  fn app() -> (AccessReport, axum::Router) {
    let admin = AdminSite::new()
      .register::<Document>(EntityAdmin::default())
      .guard(Guard::new("admin.staff", require_role("staff")))
      .router::<()>(AdminDatabase::new(
        Arc::new(TestSqlClient::new()),
        Dialect::Sqlite,
      ));
    let mcp = McpEndpoint::new(McpServer::new("docs", "1.0.0"))
      .allow_origins(OriginPolicy::allow([APP_ORIGIN]).unwrap())
      .guard(Guard::new("mcp.agent", require_role("agent")))
      .router::<()>();
    let typed: Router<()> = admin.merge(mcp);
    let report = typed.access_report();
    let axum = typed
      .into_axum()
      .layer(LocaleLayer::new(catalog()))
      .layer(axum::middleware::from_fn(authenticate));
    (report, axum)
  }

  async fn send(app: &axum::Router, req: Request<Body>) -> Response {
    app.clone().oneshot(req).await.unwrap()
  }

  fn admin(bearer: Option<&str>, accept_language: Option<&str>) -> Request<Body> {
    let mut req = Request::get("/admin");
    if let Some(bearer) = bearer {
      req = req.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    if let Some(value) = accept_language {
      req = req.header(header::ACCEPT_LANGUAGE, value);
    }
    req.body(Body::empty()).unwrap()
  }

  fn mcp(bearer: Option<&str>, origin: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut req = Request::post("/mcp")
      .header(header::CONTENT_TYPE, "application/json")
      .header("mcp-protocol-version", PROTOCOL_VERSION)
      .header("mcp-method", "server/discover");
    if let Some(bearer) = bearer {
      req = req.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    if let Some(origin) = origin {
      req = req.header(header::ORIGIN, origin);
    }
    req.body(Body::from(body)).unwrap()
  }

  fn discover(pad: usize) -> Vec<u8> {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": { "_meta": {
      "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
      "io.modelcontextprotocol/clientCapabilities": {},
      "pad": "x".repeat(pad),
    } } })
    .to_string()
    .into_bytes()
  }

  fn content_language(res: &Response) -> Option<&str> {
    res
      .headers()
      .get(header::CONTENT_LANGUAGE)
      .and_then(|v| v.to_str().ok())
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn cross_crate_cases_against_one_app() {
    let (report, app) = app();

    // R1, R7: nothing in the merged application is public; every admin route and the MCP
    // route carry their guard.
    assert!(
      report.public.is_empty(),
      "public routes: {:?}",
      report.public
    );
    assert!(report.unchecked.is_empty() && !report.unregistered_allowed);
    let policies = |path: &str| -> Vec<Vec<String>> {
      report
        .guarded
        .iter()
        .filter(|(route, _)| route.contains(path))
        .map(|(_, policies)| policies.clone())
        .collect()
    };
    let admin_routes = policies("/admin");
    assert!(admin_routes.len() >= 10, "{admin_routes:?}");
    assert!(
      admin_routes
        .iter()
        .all(|p| p.contains(&"admin.staff".to_owned()))
    );
    assert_eq!(policies("/mcp"), [vec!["mcp.agent".to_owned()]]);

    // The guard decides before either battery: anonymous is 401, the wrong role 403, and the
    // admin's guard does not admit an MCP agent nor the reverse.
    for (req, status) in [
      (admin(None, None), StatusCode::UNAUTHORIZED),
      (admin(Some("bot:agent"), None), StatusCode::FORBIDDEN),
      (mcp(None, None, discover(0)), StatusCode::UNAUTHORIZED),
      (
        mcp(Some("ada:staff"), None, discover(0)),
        StatusCode::FORBIDDEN,
      ),
    ] {
      let what = format!("{} {}", req.method(), req.uri());
      assert_eq!(send(&app, req).await.status(), status, "{what}");
    }
    assert_eq!(
      send(&app, admin(Some("ada:staff"), None)).await.status(),
      StatusCode::OK
    );

    // R9: a browser origin outside the MCP allowlist is refused even for an admitted agent; the
    // allowlisted origin and a non-browser client (no Origin) are served.
    let foreign = send(
      &app,
      mcp(Some("bot:agent"), Some("https://evil.example"), discover(0)),
    )
    .await;
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
    for origin in [Some(APP_ORIGIN), None] {
      let res = send(&app, mcp(Some("bot:agent"), origin, discover(0))).await;
      assert_eq!(res.status(), StatusCode::OK, "origin {origin:?}");
    }

    // R4: an MCP message over the 1 MiB cap is refused before it is parsed.
    let big = send(&app, mcp(Some("bot:agent"), None, discover(1024 * 1024))).await;
    assert_eq!(big.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // R4: the locale layer around both batteries reads a bounded Accept-Language and picks only
    // an allowlisted locale; an over-long header is ignored as a whole (the default).
    let de = send(&app, admin(Some("ada:staff"), Some("fr-CH, de-AT;q=0.8"))).await;
    assert_eq!(content_language(&de), Some("de"));
    let flood = format!(
      "de, {}",
      "x-a;q=0.1, ".repeat(MAX_ACCEPT_LANGUAGE_BYTES / 8)
    );
    assert!(flood.len() > MAX_ACCEPT_LANGUAGE_BYTES);
    let res = send(&app, admin(Some("ada:staff"), Some(&flood))).await;
    assert_eq!(content_language(&res), Some("en"));
    let res = send(&app, admin(Some("ada:staff"), Some("../../etc/passwd"))).await;
    assert_eq!(content_language(&res), Some("en"));
  }
}
