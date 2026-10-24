# Leptos recipe: SSR and hydration on the id_effect host

[Leptos](https://leptos.dev) is a Rust UI framework: components render to HTML on the server and
the same components, compiled to WebAssembly, *hydrate* that HTML in the browser — they attach to
the existing DOM instead of re-rendering it. This chapter puts a Leptos app on the id_effect axum
host: the pages are typed routes behind the secure stack, and the page loads its data from an
id_effect RPC service through the Rust client `#[rpc::service]` generates.

It is a **recipe**, not an integration crate (ADR 0015 D4): a complete project in
[`examples/recipes/leptos-ssr`](https://github.com/Industrial/id_effect/tree/main/examples/recipes/leptos-ssr),
outside the workspace, with its own `Cargo.lock` and its own CI job, `recipe-leptos`. Copy it and
change it. id_effect's own server-rendered UI — maud views and htmx, no WebAssembly — is
[the previous chapter](./ch29-10-server-rendered-ui.md); reach for Leptos when the page needs
client-side reactivity written in Rust.

| | |
|---|---|
| Leptos | `leptos =0.8.22`, `leptos_axum =0.8.10` (axum 0.8, the axum id_effect_axum uses) |
| Browser build | the `wasm32-unknown-unknown` target + `wasm-bindgen-cli =0.2.129` |
| npm / node_modules / bundler | none |
| Tests | `leptos_recipe_ssr_renders`, `leptos_recipe_calls_rpc` (+ the built bundle) |

## One crate, two builds

The recipe is one crate compiled twice, selected by a feature:

| Build | Feature | Target | What it contains |
|---|---|---|---|
| Server | `ssr` (default) | native | Leptos SSR through `leptos_axum`, the id_effect host, the `Notes` RPC service and its generated client |
| Browser | `hydrate` | `wasm32-unknown-unknown` | the same components and the `hydrate()` entry point; no id_effect crate at all |

`src/app.rs` is shared by both. Only the function that loads the notes differs: on the server it
calls the RPC service; in the browser it is never called while hydrating, because the server's
result travels inside the page.

```text
src/app.rs     shell(), App, NoteList, Counter — both builds
src/rpc.rs     #[rpc::service] trait Notes, the ServiceToken guard, NoteStore — server
src/server.rs  the typed routes, the security layer, NotesApi — server
src/lib.rs     hydrate() — browser
```

## The RPC service

The data lives behind an ordinary id_effect RPC service. Both procedures are **guarded** by a
service token: they serve the page renderer, a server, never the browser.

```rust
use std::sync::Arc;

use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use id_effect::{DynEnv, Effect, Never, fail, succeed};
use id_effect_axum::route::GuardFuture;
use id_effect_config::SecretString;
use id_effect_rpc as rpc;
use rpc::contract::ProcedureAccess;
use rpc::registry::RouteGuard;

#[derive(Clone, Debug, PartialEq, id_effect::Schema, serde::Serialize, serde::Deserialize)]
struct Note {
  id: i64,
  title: String,
}

#[derive(Clone, Debug, PartialEq, id_effect::Schema, serde::Serialize, serde::Deserialize)]
struct NewNote {
  title: String,
}

#[derive(Debug, PartialEq, rpc::RpcErrorUnion)]
enum NoteError {
  #[rpc(status = 422)]
  InvalidTitle { max: i64 },
}

/// The token the procedures expect; the host adds it to every request (`axum::Extension`).
#[derive(Clone)]
struct ExpectedToken(Arc<SecretString>);

/// Admits the expected token as the request's bearer token; anything else is a 401.
struct ServiceToken;

impl RouteGuard for ServiceToken {
  fn policy(&self) -> &str {
    "notes.service-token"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let presented = parts
      .headers
      .get(header::AUTHORIZATION)
      .and_then(|v| v.to_str().ok())
      .and_then(|v| v.strip_prefix("Bearer "));
    // No expected token installed: nobody gets in (fail closed). `SecretString` compares in
    // constant time.
    let admitted = match (parts.extensions.get::<ExpectedToken>(), presented) {
      (Some(ExpectedToken(expected)), Some(token)) => {
        **expected == SecretString::new(token.to_owned())
      }
      _ => false,
    };
    Box::pin(async move {
      if admitted { Ok(()) } else { Err(StatusCode::UNAUTHORIZED.into_response()) }
    })
  }
}

/// Notes for the page renderer.
#[rpc::service(version = 1)]
trait Notes {
  /// Every note, oldest first.
  #[query]
  #[guard(ServiceToken)]
  fn list(&self) -> Effect<Vec<Note>, Never, DynEnv>;

  /// Adds a note.
  #[mutation]
  #[guard(ServiceToken)]
  fn add(&self, note: NewNote) -> Effect<Note, NoteError, DynEnv>;
}

struct Store;

impl Notes for Store {
  fn list(&self) -> Effect<Vec<Note>, Never, DynEnv> {
    succeed(vec![Note { id: 1, title: "hello".into() }])
  }

  fn add(&self, note: NewNote) -> Effect<Note, NoteError, DynEnv> {
    if note.title.trim().is_empty() {
      return fail(NoteError::InvalidTitle { max: 120 });
    }
    succeed(Note { id: 2, title: note.title })
  }
}

// Every procedure carries the guard's policy in the contract.
for procedure in &NotesService::contract().procedures {
  assert_eq!(
    procedure.access,
    ProcedureAccess::Guarded { policies: vec!["notes.service-token".to_owned()] }
  );
}
# let _ = (Store, ExpectedToken(Arc::new(SecretString::new(String::new()))));
```

`#[rpc::service]` generates `NotesService` (it mounts the procedures under `POST /rpc` on a typed
router) and `NotesClient<T>`, the typed client over any transport. In the recipe, `Note` lives in
`src/app.rs` with serde derives for both builds and `id_effect::Schema` only on the server
(`#[cfg_attr(feature = "ssr", derive(id_effect::Schema))]`).

## The page

The components are plain Leptos. `App` loads the notes into a `Resource`; with
`render_app_async` the server waits for it before sending the page, and Leptos serializes the
value into the page so the browser build picks it up instead of loading again.

<!-- ignore: leptos is not a book_snippets dependency (the recipe is outside the workspace) -->
```rust,ignore
#[component]
pub fn App() -> impl IntoView {
  let notes = Resource::new(|| (), |()| load_notes());
  view! {
    <main>
      <h1>"Notes"</h1>
      <Suspense fallback=|| view! { <p>"Loading notes…"</p> }>
        {move || Suspend::new(async move {
          match notes.await {
            Ok(notes) => view! { <NoteList notes /> }.into_any(),
            Err(reason) => view! { <p class="error" role="alert">{reason}</p> }.into_any(),
          }
        })}
      </Suspense>
      <Counter />
    </main>
  }
}

/// Server: through the generated client the host provides as context.
#[cfg(feature = "ssr")]
fn load_notes() -> impl Future<Output = Result<Vec<Note>, String>> + Send + 'static {
  let api = use_context::<crate::server::NotesApi>(); // read while the owner is current
  async move {
    match api {
      Some(api) => api.list().await,
      None => Err(NOTES_UNAVAILABLE.to_owned()),
    }
  }
}

/// Browser: never called while hydrating — the server's result is in the page.
#[cfg(not(feature = "ssr"))]
async fn load_notes() -> Result<Vec<Note>, String> {
  Err(NOTES_UNAVAILABLE.to_owned())
}
```

`Counter` is a button with a signal: the server renders "Clicked 0 times"; it counts only once
the wasm module has hydrated the page. `shell()` wraps `App` in the document and adds Leptos's
`<HydrationScripts>`, which loads `/pkg/leptos_ssr.js` and calls the browser entry point:

<!-- ignore: wasm-bindgen and leptos are not book_snippets dependencies -->
```rust,ignore
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
  console_error_panic_hook::set_once();
  leptos::mount::hydrate_body(app::App);
}
```

## The host

The server side calls the RPC service through the generated client over HTTP, with the token as
`Authorization: Bearer`. The platform HTTP client keeps its SSRF guard, which refuses loopback
and private addresses; only the RPC host is allowlisted:

<!-- ignore: the recipe's NotesService lives in its own crate -->
```rust,ignore
let config = HttpClientConfig::default()
  .allow_hosts([rpc_host]) // only the RPC service's host, nothing else
  .with_timeout(RPC_TIMEOUT);
let transport = HttpTransport::new(
  RpcClient::new(base_url),
  HttpClientService::new(ReqwestHttpClient::from_config(&config)),
)
.with_bearer(token)?; // a SecretString; redacted in Debug
let client = NotesService::client(transport);
```

The page handler is an axum handler on a typed route. It takes the request's `CspNonce` from the
security layer and gives it to Leptos, replacing the nonce `leptos_axum` would generate, so the
inline scripts Leptos writes carry the nonce the CSP header names:

<!-- ignore: leptos and leptos_axum are not book_snippets dependencies -->
```rust,ignore
async fn render_page(options: LeptosOptions, api: NotesApi, nonce: CspNonce, request: Request) -> Response {
  let nonce: Arc<str> = Arc::from(nonce.as_str());
  let context = move || {
    provide_context(api.clone());
    provide_context(leptos::nonce::Nonce::from_value(Arc::clone(&nonce)));
  };
  let render = leptos_axum::render_app_async_with_context(context, move || shell(options.clone()));
  render(request).await.into_response()
}

pub fn routes(config: &RecipeConfig, store: NoteStore, api: NotesApi) -> Router<DynEnv> {
  let options = leptos_options(&config.site_root);
  let page = move |nonce: CspNonce, request: Request| {
    let (options, api) = (options.clone(), api.clone());
    async move { render_page(options, api, nonce, request).await }
  };
  NotesService::mount(Router::new(), store)                  // POST /rpc, guarded procedures
    .route(public(Method::GET, "/", get(page)))              // the page
    .route(public(Method::GET, "/pkg/{*file}", get_service(serve_dir(&config.site_root))))
}
```

`public(..)` is `Endpoint::from_parts(RouteInfo::new(method, path), handler).public()`: every
route declares its access, and the typed router answers anything else with `404`. Leptos's own
`LeptosRoutes` and `file_and_error_handler` are not used — they would mount undeclared routes.
`leptos_axum` needs a global executor for its resources; the host sets tokio's once
(`any_spawner::Executor::init_tokio()`).

## The security layer and the CSP

The application is the typed router plus `SecurityLayer::recommended`: CSRF on every unsafe
method, the security headers, and a nonce-based CSP. Two adjustments, neither of them a
`dangerous_*` opt-out:

- **`'wasm-unsafe-eval'`** in `script-src`: browsers refuse to compile WebAssembly under a CSP
  without it. It admits wasm compilation only — not JavaScript `eval`, not an inline script
  without the nonce.
- **`csrf_exempt_bearer()`**: the renderer's RPC calls carry a bearer token and no cookie, which a
  browser cannot send cross-site on its own. A call that carries a cookie is still checked.

```rust
use id_effect_axum::server::security::{ContentSecurityPolicy, CookieKey, CspNonce, SecurityLayer};

let policy = ContentSecurityPolicy::recommended().script_src(&["'wasm-unsafe-eval'"]);
let nonce = CspNonce::generate();
let header = policy.header_value_with_nonce(&nonce).expect("a valid policy");
let header = header.to_str().expect("ascii");
assert!(header.contains(&format!("script-src 'self' 'nonce-{nonce}' 'wasm-unsafe-eval'")));
assert!(header.contains("object-src 'none'"));
assert!(!header.contains("'unsafe-inline'"));

let security = SecurityLayer::recommended(CookieKey::generate())
  .csrf_exempt_bearer()
  .csp(policy)
  .expect("the policy validates");
# let _ = security;
```

The browser never holds the service token and makes no RPC call of its own. For interactive
calls from the browser, add a session-authenticated route (with the CSRF header), not the
service token.

## Building without npm

The browser half is the same crate built for `wasm32-unknown-unknown` with feature `hydrate`,
then bound by `wasm-bindgen-cli` — whose version must equal the `wasm-bindgen` in `Cargo.lock`.
`scripts/build-site.sh` checks the version and runs both steps:

```bash
cd examples/recipes/leptos-ssr
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
scripts/build-site.sh     # cargo build --lib --target wasm32-unknown-unknown --no-default-features --features hydrate
                          # wasm-bindgen --target web --out-dir target/site/pkg --out-name leptos_ssr …
cargo run                 # http://localhost:3000
```

The output is `target/site/pkg/leptos_ssr.js` and `leptos_ssr_bg.wasm` — the names
`HydrationScripts` asks for with `output_name = "leptos_ssr"`. There is no `cargo-leptos`, no
npm, no `node_modules` and no bundler. On a nightly toolchain without the target's prebuilt std
(a Nix toolchain, for instance), `BUILD_STD=1 scripts/build-site.sh` builds std from `rust-src`.

## Tests and CI

```bash
cargo nextest run --manifest-path examples/recipes/leptos-ssr/Cargo.toml
```

- `leptos_recipe_ssr_renders`: the page from a host on a loopback port — the notes from the RPC
  service, untrusted text escaped, the bundle referenced, the resolved resource in the page; the
  CSP nonce on every inline `<script>`, a fresh nonce per response, the security headers and the
  CSRF cookie; `/pkg/…` served with its media types, any undeclared path `404`; every route and
  procedure with an access decision and an empty posture registry.
- `leptos_recipe_calls_rpc`: the generated `NotesClient` over HTTP — `list`, `add`, the typed
  `NoteError`; the page shows what the service now holds; a wrong token is `401` and no token
  `403` before any procedure runs; a renderer holding the wrong token renders "unavailable" and
  no data.
- `leptos_recipe_serves_built_bundle`: the real bundle; required in CI
  (`LEPTOS_SSR_REQUIRE_SITE=1`), skipped locally when it has not been built.

The CI job `recipe-leptos` installs the pinned toolchain with the wasm target and
`wasm-bindgen-cli`, checks formatting and clippy for both builds, builds the bundle and runs the
tests. It joins `ci-pass` once it has been green twice.

This recipe is not counted toward the `web.ui` row of the scorecard (check #7, a framework
integration): it is outside the published crates.
