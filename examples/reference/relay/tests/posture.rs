//! The Relay's startup posture (ADR 0013 R10): under `prod`, with no accepted risk, every probe
//! passes or does not apply — the mesh on QUIC with mutual TLS, every procedure declared, the
//! realtime origins set, HTTP/3 with TLS, 0-RTT off, Retry on, a real certificate — and the
//! node refuses to start (before it binds a socket) when one of them would fail.

mod common;

use std::sync::Arc;

use common::{mesh_pki, node_config};
use id_effect_kit::platform::tls::NodeCertificate;
use id_effect_kit::posture::{
  CONSOLE_GUARDED, CSRF_ON, DOCS_CLOSED, HTTP3_WITH_TLS, LOAD_SHEDDING_ON, MESH_ENCRYPTED,
  NO_DEV_CERTIFICATE, NO_UNACCEPTED_OPT_OUT, PROBES, PROCEDURES_DECLARED, Posture, ProbeStatus,
  QUIC_RETRY_ON, REALTIME_ORIGINS_SET, ZERO_RTT_OFF,
};
use id_effect_kit::sql::{SqlClient, TestSqlClient};
use relay::node::RelayNode;

fn db() -> Arc<dyn SqlClient> {
  Arc::new(TestSqlClient::new())
}

fn options(origins: &[&str]) -> common::Options {
  common::Options {
    nodes: 1,
    origins: origins.iter().map(|o| (*o).to_owned()).collect(),
    ..common::Options::default()
  }
}

/// One node with `certificate`, `origins` and, when given, the startup gate of `enforce`.
async fn start(
  certificate: &NodeCertificate,
  origins: &[&str],
  enforce: Option<Posture>,
) -> Result<RelayNode, relay::node::StartError> {
  let mesh = mesh_pki(1).remove(0);
  let mut config = node_config(0, &db(), certificate, mesh, &options(origins));
  config.enforce = enforce;
  RelayNode::start(config).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_prod_posture_is_clean() {
  let certificate = NodeCertificate::self_signed(common::HOST).unwrap();
  // The gate itself: a prod node starts.
  let node = start(
    &certificate,
    &[common::APP_ORIGIN],
    Some(Posture::new("prod")),
  )
  .await
  .expect("the prod posture admits the Relay");
  let report = node.posture(Posture::new("prod")).check();
  assert!(report.passed(), "{report}");
  assert!(
    report.opt_outs().is_empty(),
    "zero accepted risks: {report}"
  );
  for probe in [
    MESH_ENCRYPTED,
    CSRF_ON,
    LOAD_SHEDDING_ON,
    PROCEDURES_DECLARED,
    REALTIME_ORIGINS_SET,
    ZERO_RTT_OFF,
    QUIC_RETRY_ON,
    HTTP3_WITH_TLS,
    NO_DEV_CERTIFICATE,
    NO_UNACCEPTED_OPT_OUT,
  ] {
    assert_eq!(
      report.probe(probe).unwrap().status,
      ProbeStatus::Pass,
      "{probe}: {report}"
    );
  }
  for probe in [CONSOLE_GUARDED, DOCS_CLOSED] {
    assert_eq!(
      report.probe(probe).unwrap().status,
      ProbeStatus::NotApplicable,
      "{probe}"
    );
  }
  // Every probe is in the report, none failed, none accepted.
  assert_eq!(report.probes().len(), PROBES.len());
  assert!(
    report
      .probes()
      .iter()
      .all(|p| p.status != ProbeStatus::Fail && p.status != ProbeStatus::Accepted)
  );
  assert!(report.enforce().is_ok());
  // Every procedure of both services declares its access: users or verified mesh nodes.
  for service in relay::contract().services {
    for procedure in &service.procedures {
      assert!(
        !procedure.access.is_public(),
        "{}.{} is public",
        service.name,
        procedure.name
      );
    }
  }
  node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_prod_refuses_to_start_on_a_failed_probe() {
  let certificate = NodeCertificate::self_signed(common::HOST).unwrap();
  // No web client origin: every browser would be refused, and prod says so.
  let refused = start(&certificate, &[], Some(Posture::new("prod")))
    .await
    .expect_err("no origin allowlist");
  assert!(refused.0.contains(REALTIME_ORIGINS_SET), "{refused}");
  // The `id-effect dev` certificate is for dev and test only (C13).
  let dir = std::env::temp_dir().join(format!("relay-dev-cert-{}", std::process::id()));
  let dev = id_effect_tool::dev::cert::ensure(&dir, std::time::SystemTime::now()).unwrap();
  let dev_cert = NodeCertificate::from_pem_files(&dev.cert, &dev.key).unwrap();
  let refused = start(&dev_cert, &[common::APP_ORIGIN], Some(Posture::new("prod")))
    .await
    .expect_err("the dev certificate in prod");
  assert!(refused.0.contains(NO_DEV_CERTIFICATE), "{refused}");
  let node = start(&dev_cert, &[common::APP_ORIGIN], Some(Posture::new("dev")))
    .await
    .expect("the dev certificate in dev");
  assert_eq!(
    node
      .posture(Posture::new("dev"))
      .check()
      .probe(NO_DEV_CERTIFICATE)
      .unwrap()
      .status,
    ProbeStatus::Pass
  );
  node.shutdown().await;
  let _ = std::fs::remove_dir_all(dir);
}
