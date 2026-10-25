//! Compile-fail and compile-pass UI tests for typed routes (H8, ADR 0013 R1).
//!
//! `fail/` pins that a route without an access decision does not compile (the
//! `#[diagnostic::on_unimplemented]` message of `route::Decided`) and that the `route_unchecked`
//! shim is deprecated; `pass/` pins what must keep compiling: public routes, guarded routes and
//! decided plain endpoints.
//!
//! # Pinned toolchain
//!
//! As in `crates/id_effect/tests/ui.rs`: the `.stderr` files are blessed on [`BLESSED_RUSTC`], the
//! version `rust-toolchain.toml` and CI pin, and the suite only runs there; on any other compiler
//! it is skipped with a note (`UI_FORCE=1` runs it anyway). Re-bless with
//! `TRYBUILD=overwrite cargo test -p id_effect_axum --test ui` when the pin moves.

/// The `rustc -V` prefix the `.stderr` snapshots were blessed with (= CI's pin).
const BLESSED_RUSTC: &str = "rustc 1.99.0";

fn rustc_version() -> String {
  let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
  std::process::Command::new(rustc)
    .arg("-V")
    .output()
    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    .unwrap_or_default()
}

#[test]
fn h8_route_without_access_decision_does_not_compile() {
  let version = rustc_version();
  if !version.starts_with(BLESSED_RUSTC) && std::env::var_os("UI_FORCE").is_none() {
    eprintln!(
      "ui: skipped on `{}`; snapshots are blessed on `{BLESSED_RUSTC}` (UI_FORCE=1 runs it)",
      version.trim()
    );
    return;
  }
  let t = trybuild::TestCases::new();
  t.compile_fail("tests/ui/fail/*.rs");
  t.pass("tests/ui/pass/*.rs");
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    if path.is_dir() {
      rust_files(&path, out);
    } else if path.extension().is_some_and(|e| e == "rs") {
      out.push(path);
    }
  }
}

/// ADR 0013 R9 (r13-e9-5-views): raw HTML only through `ui::trusted_html`. No code line in
/// `crates/id_effect_axum/src` or `crates/id_effect_admin/src` outside that module names maud's
/// unescaped wrapper or overrides `Render::render_to` (which writes into the buffer unescaped).
/// Comment lines are skipped, so docs may talk about it.
#[test]
fn no_preescaped_outside_trusted_module() {
  let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
  let trusted = std::path::Path::new("id_effect_axum/src/ui/trusted_html.rs");
  let forbidden = [concat!("Pre", "Escaped"), concat!("fn render", "_to(")];
  let mut files = Vec::new();
  for tree in ["id_effect_axum/src", "id_effect_admin/src"] {
    rust_files(&crates.join(tree), &mut files);
  }
  assert!(
    files.iter().any(|f| f.ends_with(trusted)),
    "the trusted module itself was not found"
  );
  let mut offenders = Vec::new();
  for file in files.iter().filter(|f| !f.ends_with(trusted)) {
    let text = std::fs::read_to_string(file).unwrap();
    for (n, line) in text.lines().enumerate() {
      let code = line.trim_start();
      if code.starts_with("//") {
        continue;
      }
      if forbidden.iter().any(|word| code.contains(word)) {
        offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
      }
    }
  }
  assert!(
    offenders.is_empty(),
    "raw HTML outside ui::trusted_html:\n{}",
    offenders.join("\n")
  );
}

/// Server-rendered UI (feature `ui`, r13-e9-5-views).
#[cfg(feature = "ui")]
mod views {
  use std::convert::Infallible;

  use axum::body::Body;
  use axum::http::Method;
  use axum::http::{Request, StatusCode, header};
  use axum::response::{IntoResponse, Response};
  use axum::routing::{get, post};
  use base64::Engine;
  use http_body_util::BodyExt;
  use id_effect::schema::{self, HasSchema, Schema};
  use id_effect::{Effect, succeed};
  use id_effect_axum::route::{Endpoint, Input, Route, RouteInfo, Router};
  use id_effect_axum::server::security::{CookieKey, SecurityLayer};
  use id_effect_axum::ui::htmx::{self, HxRedirect};
  use id_effect_axum::ui::{
    BasicLayout, Component, HtmlForm, LayoutLayer, Markup, Page, Slots, html,
  };
  use sha2::{Digest, Sha384};
  use tower::ServiceExt;

  async fn read(res: Response) -> (StatusCode, axum::http::HeaderMap, String) {
    let (parts, body) = res.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (
      parts.status,
      parts.headers,
      String::from_utf8(bytes.to_vec()).unwrap(),
    )
  }

  async fn call(
    app: &axum::Router,
    req: Request<Body>,
  ) -> (StatusCode, axum::http::HeaderMap, String) {
    read(app.clone().oneshot(req).await.unwrap()).await
  }

  /// Undo maud's attribute escaping (for reading attribute values back).
  fn unescape(text: &str) -> String {
    text
      .replace("&quot;", "\"")
      .replace("&lt;", "<")
      .replace("&gt;", ">")
      .replace("&amp;", "&")
  }

  /// The (escaped) value of the first `name="…"` attribute in `html`.
  fn attribute<'a>(html: &'a str, name: &str) -> &'a str {
    let marker = format!("{name}=\"");
    let start = html
      .find(&marker)
      .unwrap_or_else(|| panic!("no {name} in {html}"))
      + marker.len();
    let len = html[start..].find('"').unwrap();
    &html[start..start + len]
  }

  struct Who;

  impl HasSchema for Who {
    type A = String;
    type I = String;
    type E = ();

    fn schema() -> Schema<String, String> {
      schema::string()
    }
  }

  fn greet(_: &mut (), input: Input<(), String>) -> Effect<Markup, Infallible, ()> {
    succeed(html! { p title=(input.query) { "Hello, " (input.query) } })
  }

  #[tokio::test]
  async fn markup_route_renders_escaped() {
    let app: axum::Router = Router::new()
      .route(Route::get("/hello").public().query::<Who>().handler(greet))
      .with_state(())
      .into_axum();
    let req = Request::get("/hello?who=%3Cscript%3Ealert(%22x%22)%3C%2Fscript%3E")
      .body(Body::empty())
      .unwrap();
    let (status, headers, body) = call(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(
      body,
      "<p title=\"&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;\">Hello, \
       &lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;</p>"
    );
  }

  #[test]
  fn ui_escapes_untrusted_by_default() {
    let input = r#"<img src=x onerror="alert('&')">"#;
    let markup = html! {
      a href=(input) data-x=(input) { (input) }
      (Page::new(input, html! { (input) }).content())
    };
    let text = markup.into_string();
    assert!(!text.contains("<img"), "{text}");
    assert!(!text.contains("onerror=\""), "{text}");
    let escaped = "&lt;img src=x onerror=&quot;alert('&amp;')&quot;&gt;";
    assert_eq!(text.matches(escaped).count(), 4, "{text}");
    // The fragment of a component and a page title are escaped too.
    struct Title<'a>(&'a str);
    impl Component for Title<'_> {
      fn render(&self, slots: &Slots) -> Markup {
        html! { h1 { (self.0) (slots.children()) } }
      }
    }
    let heading = html! { (Title(input).with(Slots::new().child(input))) }.into_string();
    assert_eq!(heading.matches(escaped).count(), 2, "{heading}");
    let layout = BasicLayout::new();
    let page = id_effect_axum::ui::Layout::render(
      &layout,
      &id_effect_axum::ui::LayoutContext::default(),
      input,
      html! {},
    )
    .into_string();
    assert!(
      page.contains(&format!("<title>{escaped}</title>")),
      "{page}"
    );
  }

  struct Panel {
    heading: &'static str,
  }

  impl Component for Panel {
    fn render(&self, slots: &Slots) -> Markup {
      html! {
        section {
          header { (self.heading) (slots.named("actions")) }
          (slots.children())
          @if slots.has("footer") { footer { (slots.named("footer")) } }
        }
      }
    }
  }

  struct Badge(u32);

  impl Component for Badge {
    fn render(&self, _: &Slots) -> Markup {
      html! { span.badge { (self.0) } }
    }
  }

  #[test]
  fn component_slots_render_in_order() {
    let panel = Panel { heading: "Inbox" }.with(
      Slots::new()
        .child(html! { p { "one" } })
        .slot("actions", Badge(3).alone())
        .child(Badge(2).alone())
        .slot("footer", "f1")
        .child(html! { p { "three" } })
        .slot("footer", "f2")
        .slot("actions", "!"),
    );
    let text = html! { (panel) }.into_string();
    assert_eq!(
      text,
      "<section><header>Inbox<span class=\"badge\">3</span>!</header>\
       <p>one</p><span class=\"badge\">2</span><p>three</p><footer>f1f2</footer></section>"
    );
    // Nested: a panel inside a panel's slot.
    let outer = Panel { heading: "Outer" }
      .with(Slots::new().child(Panel { heading: "Inner" }.alone()))
      .into_response();
    assert_eq!(
      outer.headers()[header::CONTENT_TYPE],
      "text/html; charset=utf-8"
    );
  }

  fn home(_: &mut (), _: Input) -> Effect<Page, Infallible, ()> {
    succeed(Page::new("Home", html! { main #content { "welcome" } }))
  }

  fn layout_app() -> axum::Router {
    Router::new()
      .route(Route::get("/").public().handler(home))
      .route(
        Endpoint::from_parts(
          RouteInfo::new(Method::GET, "/raw"),
          get(|| async {
            html! { p { "raw" } }
          }),
        )
        .public(),
      )
      .with_state(())
      .into_axum()
      .layer(LayoutLayer::new(BasicLayout::new().site_name("Site")))
  }

  #[tokio::test]
  async fn layout_wraps_page_not_fragment() {
    let app = layout_app();
    let get = |headers: &[(&'static str, &'static str)]| {
      let mut req = Request::get("/");
      for (k, v) in headers {
        req = req.header(*k, *v);
      }
      req.body(Body::empty()).unwrap()
    };
    // A browser gets the page.
    let (status, headers, page) = call(&app, get(&[])).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
      page.starts_with("<!DOCTYPE html><html lang=\"en\">"),
      "{page}"
    );
    assert!(page.contains("<title>Home · Site</title>"));
    assert!(page.contains("<main id=\"content\">welcome</main>"));
    assert_eq!(headers[header::VARY], "hx-request");
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    // htmx gets the fragment.
    let (_, headers, fragment) = call(&app, get(&[("hx-request", "true")])).await;
    assert_eq!(fragment, "<main id=\"content\">welcome</main>");
    assert_eq!(headers[header::VARY], "hx-request");
    // A boosted navigation and a history restore swap the body: they get the page.
    for extra in ["hx-boosted", "hx-history-restore-request"] {
      let (_, _, page) = call(&app, get(&[("hx-request", "true"), (extra, "true")])).await;
      assert!(page.starts_with("<!DOCTYPE html>"), "{extra}: {page}");
    }
    // A response that is not a `Page` is left alone.
    let raw = Request::get("/raw").body(Body::empty()).unwrap();
    assert_eq!(call(&app, raw).await.2, "<p>raw</p>");
  }

  #[tokio::test]
  async fn r9_htmx_requests_carry_masked_token() {
    let app = layout_app()
      .route(
        "/notes",
        post(|| async {
          html! { p { "saved" } }
        }),
      )
      .layer(SecurityLayer::recommended(CookieKey::generate()));
    let (status, headers, page) = call(&app, Request::get("/").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    // The layout hands htmx the token for every request it sends …
    let hx_headers: serde_json::Value =
      serde_json::from_str(&unescape(attribute(&page, "hx-headers"))).unwrap();
    let token = hx_headers["x-csrf-token"].as_str().unwrap().to_owned();
    // … masked: base64url of pad || pad XOR secret, different on every response.
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
      .decode(&token)
      .unwrap();
    assert_eq!(raw.len(), 64);
    let again = Request::get("/")
      .header(header::COOKIE, &cookie)
      .body(Body::empty())
      .unwrap();
    let (_, _, second) = call(&app, again).await;
    let second: serde_json::Value =
      serde_json::from_str(&unescape(attribute(&second, "hx-headers"))).unwrap();
    let second = second["x-csrf-token"].as_str().unwrap().to_owned();
    assert_ne!(token, second, "the token is masked per response");
    // The hardened htmx config is on the page.
    let meta = &page[page.find("name=\"htmx-config\"").unwrap()..];
    let config: serde_json::Value =
      serde_json::from_str(&unescape(attribute(meta, "content"))).unwrap();
    assert_eq!(config["allowEval"], false);
    assert_eq!(config["allowScriptTags"], false);
    assert_eq!(config["selfRequestsOnly"], true);
    // … and the page's policy admits only same-origin (and nonce) scripts: the vendored file.
    let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("script-src 'self' 'nonce-"), "{csp}");
    assert!(page.contains(&format!("src=\"{}\"", htmx::HTMX_PATH)));
    // An htmx POST carrying either token passes the CSRF layer; without it, 403.
    for token in [&token, &second] {
      let post = Request::post("/notes")
        .header(header::COOKIE, &cookie)
        .header("hx-request", "true")
        .header("x-csrf-token", token.as_str())
        .body(Body::empty())
        .unwrap();
      let (status, _, body) = call(&app, post).await;
      assert_eq!(status, StatusCode::OK, "{body}");
      assert_eq!(body, "<p>saved</p>");
    }
    let forged = Request::post("/notes")
      .header(header::COOKIE, &cookie)
      .header("hx-request", "true")
      .body(Body::empty())
      .unwrap();
    assert_eq!(call(&app, forged).await.0, StatusCode::FORBIDDEN);
  }

  #[test]
  fn hx_redirect_refuses_foreign_origin() {
    for foreign in [
      "https://evil.example/",
      "http://evil.example",
      "//evil.example/path",
      "/\\evil.example",
      "\\\\evil.example",
      "javascript:alert(1)",
      "data:text/html,hi",
      "evil.example",
      "/ok\r\nSet-Cookie: x=1",
      "",
    ] {
      let refused = HxRedirect::to(foreign).unwrap_err();
      assert_eq!(refused.location, foreign);
    }
    let ok = HxRedirect::to("/todos?page=2#top").unwrap();
    let res = (ok, ()).into_response();
    assert_eq!(res.headers()["hx-redirect"], "/todos?page=2#top");
  }

  struct SignUp;

  impl HasSchema for SignUp {
    type A = (String, i64);
    type I = (String, i64);
    type E = ();

    fn schema() -> Schema<Self::A, Self::I> {
      schema::struct_(
        "name",
        schema::max_length(schema::non_empty(schema::string()), 10),
        "age",
        schema::i64(),
      )
    }
  }

  async fn sign_up(form: HtmlForm<SignUp>) -> Response {
    match form.into_result() {
      Ok(((name, age), _)) => html! { p { (name) " " (age) } }.into_response(),
      Err(state) => (
        StatusCode::UNPROCESSABLE_ENTITY,
        html! {
          form {
            (state.error_summary())
            (state.field("name", "Name"))
            (state.field("age", "Age").kind(id_effect_axum::ui::InputKind::Number))
          }
        },
      )
        .into_response(),
    }
  }

  /// The `<div class="field …" data-field="name">…</div>` of `name` in `html`.
  fn field_block<'a>(html: &'a str, name: &str) -> &'a str {
    let start = html.find(&format!("data-field=\"{name}\"")).unwrap();
    let len = html[start..].find("</div>").unwrap();
    &html[start..start + len]
  }

  #[tokio::test]
  async fn form_errors_bound_to_fields() {
    let app: axum::Router = axum::Router::new().route("/sign-up", post(sign_up));
    let submit = |body: &'static str| {
      Request::post("/sign-up")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
    };
    let (status, _, ok) = call(&app, submit("name=Ada&age=36")).await;
    assert_eq!((status, ok.as_str()), (StatusCode::OK, "<p>Ada 36</p>"));

    let (status, _, form) = call(&app, submit("name=%3CAda+Lovelace%3E&age=old")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let name = field_block(&form, "name");
    assert!(name.contains("value=\"&lt;Ada Lovelace&gt;\""), "{name}");
    assert!(name.contains("aria-invalid=\"true\""), "{name}");
    assert!(
      name.contains(
        "<li data-message-key=\"schema-must-be-at-most-n-characters\">must be at most 10 \
         characters</li>"
      ),
      "{name}"
    );
    let age = field_block(&form, "age");
    assert!(age.contains("value=\"old\""), "{age}");
    assert!(
      age.contains("data-message-key=\"schema-expected-i64\""),
      "{age}"
    );
    assert!(!name.contains("expected i64") && !age.contains("at most"));

    let (_, _, missing) = call(&app, submit("age=1")).await;
    assert!(
      field_block(&missing, "name").contains("data-message-key=\"schema-missing-field\""),
      "{missing}"
    );
  }

  #[test]
  fn htmx_asset_hash_matches_readme() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let file = std::fs::read(dir.join(format!("htmx-{}.min.js", htmx::HTMX_VERSION))).unwrap();
    assert_eq!(
      file,
      htmx::HTMX_JS,
      "the compiled-in script is the vendored file"
    );
    let digest = base64::engine::general_purpose::STANDARD.encode(Sha384::digest(&file));
    let sri = format!("sha384-{digest}");
    assert_eq!(sri, htmx::HTMX_INTEGRITY);
    let readme = std::fs::read_to_string(dir.join("README.md")).unwrap();
    assert!(
      readme.contains(&format!("`{sri}`")),
      "README does not record {sri}"
    );
    assert!(readme.contains(&format!("htmx-{}.min.js", htmx::HTMX_VERSION)));
    assert!(readme.contains("0BSD") && readme.contains("Zero-Clause BSD"));
    assert!(htmx::HTMX_VERSION.starts_with("2.0."));
  }
}
