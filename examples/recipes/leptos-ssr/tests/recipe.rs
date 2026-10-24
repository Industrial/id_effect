//! The recipe end to end, on a real loopback listener: the Leptos page rendered by the id_effect
//! host with data fetched through the generated `NotesClient`, the secure stack around it, and
//! the guarded `Notes` procedures called by that client directly.
//!
//! `leptos_recipe_serves_built_bundle` checks the real wasm bundle (`scripts/build-site.sh`); it
//! is skipped when the bundle is absent unless `LEPTOS_SSR_REQUIRE_SITE=1` (CI), which turns a
//! missing bundle into a failure.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use id_effect::{Effect, HNil};
use id_effect_axum::{ConnectionLimits, run_with_env, serve_listener};
use id_effect_config::SecretString;
use id_effect_platform::http::{HttpClientConfig, HttpClientService, ReqwestHttpClient};
use id_effect_rpc::RpcClient;
use id_effect_rpc::contract::ProcedureAccess;
use id_effect_rpc::service::{HttpTransport, RpcCallError, TransportError};
use leptos_ssr::app::NOTES_UNAVAILABLE;
use leptos_ssr::rpc::{
  MAX_TITLE, NewNote, NoteError, NoteStore, NotesService, SERVICE_TOKEN_POLICY,
};
use leptos_ssr::server::{self, NotesApi, RecipeConfig};
use tower::ServiceExt;

const TOKEN: &str = "recipe-test-service-token";

/// A site root with a stand-in bundle (the real one is checked by
/// `leptos_recipe_serves_built_bundle`).
fn stub_site() -> PathBuf {
  let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("stub-site");
  let pkg = root.join(server::PKG_DIR);
  std::fs::create_dir_all(&pkg).expect("stub site");
  std::fs::write(
    pkg.join("leptos_ssr.js"),
    "export default async function init() {}\nexport function hydrate() {}\n",
  )
  .expect("stub js");
  std::fs::write(pkg.join("leptos_ssr_bg.wasm"), b"\0asm\x01\0\0\0").expect("stub wasm");
  root
}

fn config(addr: SocketAddr, token: &str, site_root: PathBuf) -> RecipeConfig {
  RecipeConfig {
    site_root,
    service_token: SecretString::new(token.to_owned()),
    rpc_url: server::local_url(addr),
  }
}

/// A host serving `store` on a loopback port; the page renderer calls its own `/rpc`.
struct Host {
  addr: SocketAddr,
  app: axum::Router,
  config: RecipeConfig,
}

async fn start(store: NoteStore, site_root: PathBuf) -> Host {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind");
  let addr = listener.local_addr().expect("addr");
  let config = config(addr, TOKEN, site_root);
  let app = server::app(&config, store).expect("app");
  tokio::spawn(serve_listener(
    listener,
    app.clone(),
    ConnectionLimits::new(),
    std::future::pending(),
  ));
  Host { addr, app, config }
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
  let response = app.clone().oneshot(request).await.expect("infallible");
  let (parts, body) = response.into_parts();
  let body = body.collect().await.expect("body").to_bytes().to_vec();
  (parts.status, parts.headers, body)
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, HeaderMap, String) {
  let request = Request::get(uri).body(Body::empty()).expect("request");
  let (status, headers, body) = send(app, request).await;
  (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn header(headers: &HeaderMap, name: impl header::AsHeaderName) -> &str {
  headers
    .get(name)
    .map(|v| v.to_str().expect("ascii header"))
    .unwrap_or_default()
}

/// The nonce of `script-src 'nonce-…'`.
fn csp_nonce(csp: &str) -> String {
  let start = csp.find("'nonce-").expect("a nonce in the CSP") + "'nonce-".len();
  let len = csp[start..].find('\'').expect("closing quote");
  csp[start..start + len].to_owned()
}

/// Every `<script …>` start tag of `page`.
fn script_tags(page: &str) -> Vec<&str> {
  page
    .match_indices("<script")
    .map(|(at, _)| {
      let end = page[at..].find('>').expect("tag end");
      &page[at..=at + end]
    })
    .collect()
}

/// Run an effect of the generated client to its result.
async fn run<A, E>(effect: Effect<A, E, HNil>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  run_with_env(HNil, move |_| effect)
    .await
    .into_result()
    .map_err(|cause| cause.fail_option().expect("a typed failure"))
}

/// The generated client of the host at `addr`, sending `token` (or nothing).
fn client(addr: SocketAddr, token: Option<&str>) -> leptos_ssr::rpc::NotesClient<HttpTransport> {
  let config = HttpClientConfig::default().allow_hosts(["127.0.0.1"]);
  let transport = HttpTransport::new(
    RpcClient::new(server::local_url(addr)),
    HttpClientService::new(ReqwestHttpClient::from_config(&config)),
  );
  let transport = match token {
    Some(token) => transport
      .with_bearer(SecretString::new(token.to_owned()))
      .expect("a valid token"),
    None => transport,
  };
  NotesService::client(transport)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leptos_recipe_ssr_renders() {
  let store = NoteStore::with_titles(["First note", "<script>alert(1)</script>"]);
  let host = start(store, stub_site()).await;

  // The document, rendered by Leptos on the id_effect host.
  let (status, headers, page) = get(&host.app, "/").await;
  assert_eq!(status, StatusCode::OK, "{page}");
  assert!(header(&headers, header::CONTENT_TYPE).starts_with("text/html"));
  assert!(page.starts_with("<!DOCTYPE html>"), "{page}");
  assert!(
    page.contains("<title>Notes — id_effect + Leptos</title>"),
    "{page}"
  );

  // The data came through the RPC call made while rendering; untrusted text is escaped.
  assert!(page.contains(">First note</li>"), "{page}");
  assert!(
    page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
    "{page}"
  );
  assert!(!page.contains("<script>alert(1)"), "{page}");
  assert!(!page.contains(NOTES_UNAVAILABLE), "{page}");
  // The counter, server-rendered at zero (it counts once hydrated).
  assert!(page.contains("id=\"counter\""), "{page}");

  // Hydration: the bundle is referenced, and the resolved resource travels in the page for the
  // browser build to pick up instead of calling the service itself.
  assert!(page.contains("/pkg/leptos_ssr.js"), "{page}");
  assert!(page.contains("/pkg/leptos_ssr_bg.wasm"), "{page}");
  assert!(page.contains("__RESOLVED_RESOURCES"), "{page}");
  assert!(page.contains("First note"), "{page}");

  // The 0.11 secure stack: a nonce CSP (plus wasm compilation), every inline script carrying
  // this response's nonce, the security headers and the CSRF cookie.
  let csp = header(&headers, header::CONTENT_SECURITY_POLICY);
  for directive in [
    "default-src 'self'",
    "object-src 'none'",
    "frame-ancestors 'none'",
    "'wasm-unsafe-eval'",
  ] {
    assert!(csp.contains(directive), "{directive} missing from {csp}");
  }
  assert!(
    !csp.contains("'unsafe-inline'") && !csp.contains("'unsafe-eval'"),
    "{csp}"
  );
  let nonce = csp_nonce(csp);
  let scripts = script_tags(&page);
  assert!(!scripts.is_empty(), "{page}");
  for tag in &scripts {
    assert!(
      tag.contains(&format!("nonce=\"{nonce}\"")),
      "{tag} lacks the CSP nonce"
    );
  }
  assert_eq!(header(&headers, header::X_FRAME_OPTIONS), "DENY");
  assert_eq!(header(&headers, header::X_CONTENT_TYPE_OPTIONS), "nosniff");
  assert!(header(&headers, header::SET_COOKIE).starts_with("__Host-"));

  // A fresh nonce per response.
  let (_, again, _) = get(&host.app, "/").await;
  assert_ne!(
    csp_nonce(header(&again, header::CONTENT_SECURITY_POLICY)),
    nonce
  );

  // The bundle and the stylesheet are public routes; nothing else is served.
  let (status, headers, _) = get(&host.app, "/pkg/leptos_ssr_bg.wasm").await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(header(&headers, header::CONTENT_TYPE), "application/wasm");
  let (status, headers, js) = get(&host.app, "/pkg/leptos_ssr.js").await;
  assert_eq!(status, StatusCode::OK);
  assert!(header(&headers, header::CONTENT_TYPE).contains("javascript"));
  assert!(js.contains("hydrate"));
  let (status, headers, _) = get(&host.app, server::STYLESHEET_PATH).await;
  assert_eq!(status, StatusCode::OK);
  assert!(header(&headers, header::CONTENT_TYPE).starts_with("text/css"));
  for path in ["/pkg/missing.js", "/pkg/../Cargo.toml", "/admin", "/rpc"] {
    let (status, _, body) = get(&host.app, path).await;
    assert!(
      matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
      ),
      "{path}: {status} {body}"
    );
  }

  // Every route declared its access: pages and assets public, the procedures guarded, and no
  // `dangerous_*` opt-out anywhere in the process.
  let api = NotesApi::connect(&host.config.rpc_url, host.config.service_token.clone()).unwrap();
  let routes = server::routes(&host.config, NoteStore::new(), api);
  let report = routes.access_report();
  assert!(!report.has_opt_outs(), "{report:?}");
  let paths: Vec<&str> = routes.routes().map(|r| r.path.as_str()).collect();
  for path in ["/", "/pkg/{*file}", server::STYLESHEET_PATH, "/rpc"] {
    assert!(paths.contains(&path), "{path} not declared: {paths:?}");
  }
  for procedure in &NotesService::contract().procedures {
    assert_eq!(
      procedure.access,
      ProcedureAccess::Guarded {
        policies: vec![SERVICE_TOKEN_POLICY.to_owned()]
      },
      "{}",
      procedure.name
    );
  }
  assert!(
    id_effect::observability::posture::dangerous_opt_outs().is_empty(),
    "{:?}",
    id_effect::observability::posture::dangerous_opt_outs()
  );
  let _ = host.addr;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leptos_recipe_calls_rpc() {
  let host = start(NoteStore::with_titles(["seed"]), stub_site()).await;

  // The generated client, with the service token: both procedures.
  let notes = client(host.addr, Some(TOKEN));
  let listed = run(notes.list()).await.expect("list");
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].title, "seed");
  let added = run(notes.add(NewNote {
    title: "  added through NotesClient  ".to_owned(),
  }))
  .await
  .expect("add");
  assert_eq!(
    (added.id, added.title.as_str()),
    (2, "added through NotesClient")
  );
  let refused = run(notes.add(NewNote {
    title: "   ".to_owned(),
  }))
  .await
  .expect_err("an empty title");
  assert_eq!(
    refused.typed(),
    Some(&NoteError::InvalidTitle {
      max: MAX_TITLE as i64
    })
  );

  // The page renders what the service now holds: its data comes through the same client.
  let (status, _, page) = get(&host.app, "/").await;
  assert_eq!(status, StatusCode::OK, "{page}");
  assert!(page.contains(">seed</li>"), "{page}");
  assert!(page.contains(">added through NotesClient</li>"), "{page}");

  // Refused before any procedure runs: a wrong token by the `ServiceToken` guard (401); no
  // token at all already by CSRF (403), since only bearer calls skip the CSRF check.
  for (token, status) in [(Some("not-the-token"), 401), (None, 403)] {
    let outsider = client(host.addr, token);
    let err = run(outsider.list()).await.expect_err("refused");
    assert!(
      matches!(err, RpcCallError::Transport(TransportError::Refused { status: s, .. }) if s == status),
      "{token:?}: {err:?}"
    );
    let err = run(outsider.add(NewNote {
      title: "sneaky".to_owned(),
    }))
    .await
    .expect_err("refused");
    assert!(
      matches!(err, RpcCallError::Transport(TransportError::Refused { status: s, .. }) if s == status),
      "{token:?}: {err:?}"
    );
  }
  assert_eq!(run(notes.list()).await.expect("list").len(), 2);

  // A browser-shaped call (a cookie, no CSRF token) is stopped by CSRF before the guard, even
  // with the right bearer token: only cookie-less bearer calls skip the CSRF check.
  let body = serde_json::json!({"tag": "Notes.list", "payload": null}).to_string();
  let browser = Request::post("/rpc")
    .header(header::CONTENT_TYPE, "application/json")
    .header("x-rpc", "1")
    .header(header::COOKIE, "session=1")
    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
    .body(Body::from(body))
    .expect("request");
  let (status, _, _) = send(&host.app, browser).await;
  assert_eq!(status, StatusCode::FORBIDDEN);

  // A renderer holding the wrong token gets nothing from the service: the page says so (fail
  // closed) and shows no note.
  let impostor = server::app(
    &config(host.addr, "not-the-token", stub_site()),
    NoteStore::new(),
  )
  .expect("app");
  let (status, _, page) = get(&impostor, "/").await;
  assert_eq!(status, StatusCode::OK, "{page}");
  assert!(page.contains(NOTES_UNAVAILABLE), "{page}");
  assert!(!page.contains(">seed</li>"), "{page}");
}

/// The bundle `scripts/build-site.sh` wrote (`LEPTOS_SSR_SITE_ROOT`, default `target/site`).
fn built_site() -> PathBuf {
  std::env::var_os("LEPTOS_SSR_SITE_ROOT")
    .map(PathBuf::from)
    .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/site"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leptos_recipe_serves_built_bundle() {
  let site = built_site();
  let js_path = site.join("pkg/leptos_ssr.js");
  let required = std::env::var("LEPTOS_SSR_REQUIRE_SITE").is_ok_and(|v| v == "1");
  if !js_path.is_file() {
    assert!(
      !required,
      "no bundle at {}: run scripts/build-site.sh",
      site.display()
    );
    eprintln!(
      "SKIP leptos_recipe_serves_built_bundle: no bundle at {}",
      site.display()
    );
    return;
  }
  let host = start(NoteStore::seeded(), site.clone()).await;

  // wasm-bindgen's ES module exports `hydrate` (the `#[wasm_bindgen]` entry point) and the
  // module is real WebAssembly, both served from this origin with their media types.
  let (status, headers, js) = get(&host.app, "/pkg/leptos_ssr.js").await;
  assert_eq!(status, StatusCode::OK);
  assert!(header(&headers, header::CONTENT_TYPE).contains("javascript"));
  assert!(js.contains("export function hydrate"), "{js}");
  let request = Request::get("/pkg/leptos_ssr_bg.wasm")
    .body(Body::empty())
    .expect("request");
  let (status, headers, wasm) = send(&host.app, request).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(header(&headers, header::CONTENT_TYPE), "application/wasm");
  assert!(wasm.starts_with(b"\0asm"), "not a wasm module");
  assert_eq!(
    wasm.len() as u64,
    std::fs::metadata(site.join("pkg/leptos_ssr_bg.wasm"))
      .expect("wasm file")
      .len()
  );

  // The page points the hydration script at exactly these files.
  let (_, _, page) = get(&host.app, "/").await;
  assert!(page.contains("\"leptos_ssr\""), "{page}");
  assert!(page.contains("\"leptos_ssr_bg\""), "{page}");
}

/// Set to `1` to run `leptos_recipe_hydrates_in_browser` (Chromium on `PATH` and the built
/// bundle required; a missing one fails the test instead of skipping it).
const REQUIRE_BROWSER: &str = "ID_EFFECT_REQUIRE_BROWSER";

/// `#counter`'s text after clicking it, from inside the page.
const CLICK_COUNTER: &str =
  "(() => { const b = document.getElementById('counter'); b.click(); return b.textContent; })()";

/// The clicks `Clicked N times` reports.
fn clicks(text: &str) -> u32 {
  text
    .trim()
    .strip_prefix("Clicked ")
    .and_then(|rest| rest.strip_suffix(" times"))
    .and_then(|n| n.trim().parse().ok())
    .unwrap_or_else(|| panic!("unexpected counter text {text:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leptos_recipe_hydrates_in_browser() {
  use std::time::{Duration, Instant};

  use headless_chrome::{Browser, LaunchOptions};

  if std::env::var(REQUIRE_BROWSER).as_deref() != Ok("1") {
    eprintln!("SKIP leptos_recipe_hydrates_in_browser: set {REQUIRE_BROWSER}=1 (Chromium on PATH)");
    return;
  }
  let site = built_site();
  assert!(
    site.join("pkg/leptos_ssr_bg.wasm").is_file(),
    "no bundle at {}: run scripts/build-site.sh",
    site.display()
  );
  let host = start(NoteStore::seeded(), site).await;
  let url = format!("http://{}/", host.addr);

  // Headless Chromium loads the page under the host's real CSP: the nonce'd module script must
  // run, the wasm must compile (`'wasm-unsafe-eval'`), and the counter must start counting.
  let (before, after, notes) = tokio::task::spawn_blocking(move || {
    let options = LaunchOptions::default_builder()
      .headless(true)
      .sandbox(false)
      .idle_browser_timeout(Duration::from_secs(120))
      .build()
      .expect("launch options");
    let browser = Browser::new(options).expect("Chromium on PATH (ID_EFFECT_REQUIRE_BROWSER=1)");
    let tab = browser.new_tab().expect("tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("navigated");
    let text = |script: &str| {
      tab
        .evaluate(script, false)
        .expect("evaluate")
        .value
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
    };
    // Before hydration a click does nothing; poll until one counts.
    let deadline = Instant::now() + Duration::from_secs(60);
    let before = loop {
      let shown = clicks(&text(CLICK_COUNTER));
      if shown > 0 {
        break shown;
      }
      assert!(Instant::now() < deadline, "the page never hydrated: {url}");
      std::thread::sleep(Duration::from_millis(100));
    };
    let after = clicks(&text(CLICK_COUNTER));
    let notes = text(
      "Array.from(document.querySelectorAll('#notes li')).map(li => li.textContent).join('|')",
    );
    (before, after, notes)
  })
  .await
  .expect("browser thread");

  // Hydrated: each click counts, and the server-rendered notes survived hydration intact.
  assert_eq!(after, before + 1);
  assert_eq!(
    notes,
    "Rendered by Leptos on the id_effect host|Loaded through the generated NotesClient|\
     Hydrated in the browser by the wasm build"
  );
}
