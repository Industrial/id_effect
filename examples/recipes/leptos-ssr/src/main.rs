//! The recipe server: Leptos SSR + hydration and the `Notes` RPC service on one id_effect host.
//!
//! ```text
//! scripts/build-site.sh                          # the wasm bundle → target/site/pkg
//! cargo run                                      # http://localhost:3000
//! ```
//!
//! Environment: `LEPTOS_SSR_ADDR` (default `127.0.0.1:3000`), `LEPTOS_SSR_SITE_ROOT` (default
//! `target/site`), `NOTES_SERVICE_TOKEN` (default: a random token per start — the renderer and
//! the service are the same process here).

use std::net::SocketAddr;
use std::path::PathBuf;

use id_effect_config::SecretString;
use leptos_ssr::rpc::NoteStore;
use leptos_ssr::server::{self, RecipeConfig};

#[tokio::main]
async fn main() -> std::io::Result<()> {
  let addr: SocketAddr = std::env::var("LEPTOS_SSR_ADDR")
    .unwrap_or_else(|_| "127.0.0.1:3000".to_owned())
    .parse()
    .map_err(std::io::Error::other)?;
  let site_root = std::env::var_os("LEPTOS_SSR_SITE_ROOT")
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("target/site"));
  let service_token = std::env::var("NOTES_SERVICE_TOKEN")
    .map(SecretString::new)
    .unwrap_or_else(|_| server::generate_service_token());

  let listener = tokio::net::TcpListener::bind(addr).await?;
  let local = listener.local_addr()?;
  if !site_root.join(server::PKG_DIR).is_dir() {
    eprintln!(
      "leptos_ssr: no {}/{} — run scripts/build-site.sh for hydration (the page still renders)",
      site_root.display(),
      server::PKG_DIR
    );
  }
  let config = RecipeConfig {
    site_root,
    service_token,
    rpc_url: server::local_url(local),
  };
  // The CSRF cookie is `__Host-` and `Secure`: browsers accept it on http://localhost.
  println!("http://localhost:{}/", local.port());
  server::serve(listener, config, NoteStore::seeded(), async {
    let _ = tokio::signal::ctrl_c().await;
  })
  .await
}
