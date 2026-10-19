//! `relay`: one Relay node, configured from the environment ([`relay::config`]), until SIGINT /
//! SIGTERM.
//!
//! Before it binds a socket the node runs the security posture check (ADR 0013 R10) under the
//! profile `ID_EFFECT_PROFILE` selects (default `dev`); under `prod` a failed probe stops it.
//! Then it joins every node of `RELAY_PEERS` (the others join it the same way).

use std::process::ExitCode;
use std::sync::Arc;

use id_effect::HNil;
use id_effect_kit::axum::run_with_env;
use id_effect_kit::axum::server::security::{COOKIE_KEY_ENV, CookieKey};
use id_effect_kit::config::Profiles;
use id_effect_kit::node::Cookie;
use id_effect_kit::platform::tls::NodeCertificate;
use id_effect_kit::posture::Posture;
use id_effect_kit::rpc::channels::ChannelsConfig;
use id_effect_kit::sql::SqlClient;
use id_effect_kit::sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use relay::auth::TokenKeys;
use relay::config::Settings;
use relay::node::{MeshTls, NodeConfig, RelayNode};
use relay::rooms::Rooms;

#[tokio::main]
async fn main() -> ExitCode {
  match run().await {
    Ok(()) => {
      eprintln!("stopped");
      ExitCode::SUCCESS
    }
    Err(e) => {
      eprintln!("error: {e}");
      ExitCode::FAILURE
    }
  }
}

async fn run() -> Result<(), String> {
  let stop = shutdown_signal();
  let settings = Settings::from_env()?;
  let base = Posture::load(&Profiles::new()).map_err(|e| e.to_string())?;
  let certificate = NodeCertificate::from_pem_files(&settings.tls.0, &settings.tls.1)
    .map_err(|e| format!("TLS_CERT_FILE / TLS_KEY_FILE: {e}"))?;
  let mesh_certificate =
    NodeCertificate::from_pem_files(&settings.mesh_tls.0, &settings.mesh_tls.1)
      .map_err(|e| format!("RELAY_MESH_CERT_FILE / RELAY_MESH_KEY_FILE: {e}"))?;
  let keys = TokenKeys::new(
    settings.token_secret.clone().into_bytes(),
    settings.token_issuer.clone(),
  )?;
  let pool = pg_pool_from_config(PgPoolConfig::from_url(&settings.database_url))
    .await
    .map_err(|e| format!("RELAY_DATABASE_URL: {}", e.0))?;
  let db: Arc<dyn SqlClient> = Arc::new(PgSqlClient::new(pool));
  let migrate = Rooms::new(Arc::clone(&db)).migrate();
  run_with_env(HNil, move |_| migrate)
    .await
    .into_result()
    .map_err(|cause| format!("schema: {cause:?}"))?;
  let node = RelayNode::start(NodeConfig {
    name: settings.node.clone(),
    bind: settings.bind,
    certificate,
    mesh: MeshTls {
      certificate: mesh_certificate,
      trusted: settings.mesh_trusted.clone(),
    },
    cookie: Cookie::new(settings.mesh_cookie.clone()),
    origins: settings.origins.clone(),
    keys,
    db,
    cookie_key: cookie_key()?,
    channels: ChannelsConfig::new(),
    advertise_h3_port: None,
    enforce: Some(base.clone()),
  })
  .await
  .map_err(|e| e.to_string())?;
  eprint!("{}", node.posture(base).check());
  eprintln!("{} on {} (TCP and UDP)", node.name(), node.addr());
  let join_peers = async {
    for peer in &settings.peers {
      match node.connect(*peer).await {
        Ok(name) => eprintln!("joined {name} at {peer}"),
        Err(e) => eprintln!("{peer}: {e} (it joins us when it starts)"),
      }
    }
  };
  // A signal while the peers are being joined stops the node as well.
  let mut stop = std::pin::pin!(stop);
  tokio::select! {
    () = join_peers => stop.await,
    () = &mut stop => {}
  }
  node.shutdown().await;
  Ok(())
}

/// The cookie signing key from `COOKIE_KEY` (standard base64, at least 64 bytes); a random one
/// without it.
fn cookie_key() -> Result<CookieKey, String> {
  match std::env::var(COOKIE_KEY_ENV) {
    Ok(encoded) if !encoded.trim().is_empty() => {
      CookieKey::from_base64(&encoded).map_err(|e| format!("{COOKIE_KEY_ENV}: {e}"))
    }
    _ => Ok(CookieKey::generate()),
  }
}

/// SIGINT or SIGTERM. The SIGTERM handler is installed when this is called (not when the future
/// is first polled), so a signal that arrives while the node starts or joins its peers stops it
/// cleanly too.
fn shutdown_signal() -> impl std::future::Future<Output = ()> {
  #[cfg(unix)]
  let term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
  async move {
    #[cfg(unix)]
    if let Some(mut term) = term {
      tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
      }
      return;
    }
    let _ = tokio::signal::ctrl_c().await;
  }
}
