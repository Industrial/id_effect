//! RPC tracing + API versioning example with OTEL starter.
//!
//! ```bash
//! cargo run -p id_effect_rpc --example 020_rpc_tracing
//! ```

use axum::Router;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use id_effect_opentelemetry::{
  OtelInMemoryExporters, OtelProviders, OtelStarterConfig, install_otel_starter,
};
use id_effect_rpc::correlation::{append_correlation_header, ensure_correlation_id};
use id_effect_rpc::span::{record_request_metadata, rpc_request_span};
use id_effect_rpc::versioning::{ApiVersion, VersionConfig, negotiate_api_version};
use tracing::info;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
  let exporters = OtelInMemoryExporters::default();
  let providers = OtelProviders::with_in_memory_exporters(&exporters);
  let config = OtelStarterConfig::new("rpc_tracing_example");
  let _guard = install_otel_starter(&providers, &config).expect("otel");

  let version_config = VersionConfig::new(ApiVersion::new("v1"), vec![ApiVersion::new("v1")]);

  let app = Router::new()
    .route(
      "/v1/echo",
      post(|headers: axum::http::HeaderMap, body: Bytes| async move {
        let cid = ensure_correlation_id(&headers);
        let span = rpc_request_span();
        let _enter = span.enter();
        record_request_metadata(&span, Some(cid.as_str()), "POST", "/v1/echo", "echo");
        let text = String::from_utf8_lossy(&body).into_owned();
        info!(payload = %text, "rpc echo");
        let mut res = (StatusCode::OK, text).into_response();
        append_correlation_header(res.headers_mut(), &cid);
        res
      }),
    )
    .layer(axum::middleware::from_fn_with_state(
      version_config,
      negotiate_api_version,
    ));

  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  println!("RPC tracing example: http://{addr}/v1/echo");
  axum::serve(listener, app).await.unwrap();
}
