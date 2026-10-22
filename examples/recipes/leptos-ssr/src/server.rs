//! The host: one `id_effect_axum::route::Router` serving
//!
//! - `GET /` — the Leptos page, server-rendered by `leptos_axum` (**public**);
//! - `GET /pkg/{*file}` — the wasm bundle the hydration script loads (**public**);
//! - `GET /recipe.css` — the stylesheet (**public**);
//! - `POST /rpc` — the [`Notes`](crate::rpc::Notes) procedures (**guarded** by
//!   [`ServiceToken`](crate::rpc::ServiceToken));
//!
//! behind `SecurityLayer::recommended`: CSRF on every unsafe method (the page's server-side RPC
//! calls carry a bearer token and no cookie, so `csrf_exempt_bearer` lets exactly those through),
//! the recommended security headers, and a nonce CSP whose nonce Leptos puts on its inline
//! scripts. Anything else is a `404` (the typed router's deny-by-default layer). No
//! `dangerous_*` opt-out is taken.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Request;
use axum::http::{HeaderValue, Method, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, get_service};
use axum::{Extension, Router as AxumRouter};
use id_effect::{DynEnv, HNil};
use id_effect_axum::route::{Endpoint, Public, RouteInfo, Router};
use id_effect_axum::server::security::{
  ContentSecurityPolicy, CookieKey, CspNonce, SecurityError, SecurityLayer,
};
use id_effect_axum::{ConnectionLimits, run_with_env, serve_dir, serve_listener};
use id_effect_config::SecretString;
use id_effect_platform::http::{HttpClientConfig, HttpClientService, ReqwestHttpClient};
use id_effect_rpc::RpcClient;
use id_effect_rpc::service::{HttpHeaderError, HttpTransport};
use leptos::config::LeptosOptions;
use leptos::nonce::Nonce;
use leptos::prelude::provide_context;

use crate::app::{NOTES_UNAVAILABLE, NotesView, shell};
use crate::rpc::{ExpectedToken, NoteStore, NotesClient, NotesService};

/// The wasm-bindgen output name: `/pkg/leptos_ssr.js` and `/pkg/leptos_ssr_bg.wasm`.
pub const OUTPUT_NAME: &str = "leptos_ssr";

/// The directory under the site root holding the wasm bundle (`/pkg/…`).
pub const PKG_DIR: &str = "pkg";

/// Where the stylesheet is served.
pub const STYLESHEET_PATH: &str = "/recipe.css";

/// The stylesheet (a file in the crate, served from memory).
const STYLESHEET: &str = include_str!("../assets/recipe.css");

/// How long one server-side RPC call may take before the page renders without its data.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// What the host needs.
#[derive(Clone, Debug)]
pub struct RecipeConfig {
  /// The directory holding `pkg/` (`scripts/build-site.sh` writes `target/site/pkg`).
  pub site_root: PathBuf,
  /// The token the `Notes` procedures expect, and that the page renderer sends.
  pub service_token: SecretString,
  /// Where the page renderer reaches the `Notes` service: this host (`http://127.0.0.1:<port>`)
  /// in the recipe; a separate backend works the same way.
  pub rpc_url: String,
}

/// Why the host could not be built.
#[derive(Debug)]
pub enum RecipeError {
  /// The RPC URL is not an absolute `http(s)` URL with a host.
  InvalidRpcUrl(String),
  /// The service token is not a valid header value.
  InvalidToken(HttpHeaderError),
  /// The CSP did not validate.
  Security(SecurityError),
}

impl std::fmt::Display for RecipeError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::InvalidRpcUrl(url) => write!(f, "invalid RPC URL {url:?}"),
      Self::InvalidToken(e) => write!(f, "invalid service token: {e}"),
      Self::Security(e) => write!(f, "security configuration: {e}"),
    }
  }
}

impl std::error::Error for RecipeError {}

/// The `Notes` client the page renders with: the generated [`NotesClient`] over HTTP, carrying
/// the service token as `Authorization: Bearer`. Provided to Leptos as context per request.
#[derive(Clone, Debug)]
pub struct NotesApi {
  client: Arc<NotesClient<HttpTransport>>,
}

impl NotesApi {
  /// A client of the service at `base_url` (its `/rpc` endpoint) sending `token`.
  ///
  /// The platform HTTP client's SSRF guard refuses loopback and private addresses; the RPC
  /// host — and only it — is allowlisted (`allow_hosts`), so the renderer can reach a service on
  /// the same machine without opening the guard for anything else.
  pub fn connect(base_url: &str, token: SecretString) -> Result<Self, RecipeError> {
    let host = rpc_host(base_url)?;
    let config = HttpClientConfig::default()
      .allow_hosts([host])
      .with_timeout(RPC_TIMEOUT);
    let transport = HttpTransport::new(
      RpcClient::new(base_url),
      HttpClientService::new(ReqwestHttpClient::from_config(&config)),
    )
    .with_bearer(token)
    .map_err(RecipeError::InvalidToken)?;
    Ok(Self {
      client: Arc::new(NotesService::client(transport)),
    })
  }

  /// The generated client itself.
  pub fn client(&self) -> &NotesClient<HttpTransport> {
    &self.client
  }

  /// `Notes.list`, for the page: the notes, or [`NOTES_UNAVAILABLE`] (the cause goes to stderr,
  /// never into the HTML).
  pub async fn list(&self) -> NotesView {
    let client = Arc::clone(&self.client);
    run_with_env(HNil, move |_| client.list::<HNil>())
      .await
      .into_result()
      .map_err(|cause| {
        eprintln!("leptos_ssr: Notes.list failed: {cause:?}");
        NOTES_UNAVAILABLE.to_owned()
      })
  }
}

/// The host part of `url`, for the SSRF allowlist.
fn rpc_host(url: &str) -> Result<String, RecipeError> {
  let invalid = || RecipeError::InvalidRpcUrl(url.to_owned());
  let uri: Uri = url.parse().map_err(|_| invalid())?;
  match uri.scheme_str() {
    Some("http" | "https") => {}
    _ => return Err(invalid()),
  }
  uri.host().map(str::to_owned).ok_or_else(invalid)
}

/// The Leptos options for `site_root`: output name [`OUTPUT_NAME`], bundle under [`PKG_DIR`].
pub fn leptos_options(site_root: &Path) -> LeptosOptions {
  LeptosOptions::builder()
    .output_name(OUTPUT_NAME)
    .site_root(site_root.to_string_lossy().into_owned())
    .site_pkg_dir(PKG_DIR)
    .build()
}

/// The CSP: [`ContentSecurityPolicy::recommended`] (`script-src 'self' 'nonce-…'`,
/// `default-src 'self'`, `object-src 'none'`, …) plus `'wasm-unsafe-eval'`, which browsers
/// require before they compile WebAssembly under a CSP. It admits wasm compilation only — not
/// `eval` of JavaScript, not inline scripts without the nonce.
pub fn content_security_policy() -> ContentSecurityPolicy {
  ContentSecurityPolicy::recommended().script_src(&["'wasm-unsafe-eval'"])
}

/// The typed routes (before the security layer): pages and assets public, the RPC procedures
/// guarded. Exposed so tests can read its access report.
pub fn routes(config: &RecipeConfig, store: NoteStore, api: NotesApi) -> Router<DynEnv> {
  let options = leptos_options(&config.site_root);
  let page = move |nonce: CspNonce, request: Request| {
    let options = options.clone();
    let api = api.clone();
    async move { render_page(options, api, nonce, request).await }
  };
  NotesService::mount(Router::new(), store)
    .route(public(Method::GET, "/", get(page)))
    .route(public(
      Method::GET,
      "/pkg/{*file}",
      get_service(serve_dir(&config.site_root)),
    ))
    .route(public(Method::GET, STYLESHEET_PATH, get(stylesheet)))
}

/// The whole application: [`routes`] with the expected service token and the secure stack.
pub fn app(config: &RecipeConfig, store: NoteStore) -> Result<AxumRouter, RecipeError> {
  // Leptos spawns its resource futures on a global executor: tokio's, the one the host runs
  // on. Set once per process; a second call keeps the first.
  let _ = any_spawner::Executor::init_tokio();
  let api = NotesApi::connect(&config.rpc_url, config.service_token.clone())?;
  let security = SecurityLayer::recommended(CookieKey::generate())
    // The renderer's RPC calls: bearer token, no cookie (a browser cannot forge those).
    .csrf_exempt_bearer()
    .csp(content_security_policy())
    .map_err(RecipeError::Security)?;
  Ok(
    routes(config, store, api)
      .with_state(DynEnv::new())
      .into_axum()
      .layer(Extension(ExpectedToken::new(config.service_token.clone())))
      .layer(security),
  )
}

/// Serve [`app`] on `listener` until `shutdown` resolves.
pub async fn serve(
  listener: tokio::net::TcpListener,
  config: RecipeConfig,
  store: NoteStore,
  shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
  let app = app(&config, store).map_err(std::io::Error::other)?;
  serve_listener(listener, app, ConnectionLimits::new(), shutdown).await
}

/// `http://<addr>`: the RPC URL of a host listening on `addr`.
pub fn local_url(addr: SocketAddr) -> String {
  format!("http://{addr}")
}

/// A typed endpoint anyone may call, from a plain method router.
fn public(
  method: Method,
  path: &str,
  handler: axum::routing::MethodRouter<DynEnv>,
) -> Endpoint<DynEnv, Public> {
  Endpoint::from_parts(RouteInfo::new(method, path), handler).public()
}

/// Render the page with Leptos: every resource resolves first (`render_app_async`), then the
/// whole document is sent. Leptos gets this request's CSP nonce and the [`NotesApi`].
async fn render_page(
  options: LeptosOptions,
  api: NotesApi,
  nonce: CspNonce,
  request: Request,
) -> Response {
  let nonce: Arc<str> = Arc::from(nonce.as_str());
  let context = move || {
    provide_context(api.clone());
    // Replaces the nonce leptos_axum generated: Leptos must use the one in the CSP header.
    provide_context(Nonce::from_value(Arc::clone(&nonce)));
  };
  let render = leptos_axum::render_app_async_with_context(context, move || shell(options.clone()));
  render(request).await.into_response()
}

async fn stylesheet() -> Response {
  (
    [
      (
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
      ),
      (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
    ],
    STYLESHEET,
  )
    .into_response()
}

/// A random service token (256 bits, hex) for a host configured without one.
pub fn generate_service_token() -> SecretString {
  use rand::RngCore;
  let mut bytes = [0u8; 32];
  rand::rng().fill_bytes(&mut bytes);
  SecretString::new(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
