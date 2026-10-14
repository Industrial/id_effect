//! Axum mounts for Process Mesh control plane (optional `cluster` feature).

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use id_effect_node::{ClusterStatus, DrainRequest, RebalanceRequest};

/// Mutable status + admin hooks.
#[derive(Clone)]
pub struct ClusterControl {
  status: Arc<Mutex<ClusterStatus>>,
  on_drain: Arc<dyn Fn(DrainRequest) + Send + Sync>,
  on_rebalance: Arc<dyn Fn(RebalanceRequest) + Send + Sync>,
}

impl ClusterControl {
  /// Snapshot-only control (admin actions are no-ops).
  pub fn snapshot(status: ClusterStatus) -> Self {
    Self {
      status: Arc::new(Mutex::new(status)),
      on_drain: Arc::new(|_| {}),
      on_rebalance: Arc::new(|_| {}),
    }
  }

  /// Full control with callbacks.
  pub fn with_hooks(
    status: ClusterStatus,
    on_drain: impl Fn(DrainRequest) + Send + Sync + 'static,
    on_rebalance: impl Fn(RebalanceRequest) + Send + Sync + 'static,
  ) -> Self {
    Self {
      status: Arc::new(Mutex::new(status)),
      on_drain: Arc::new(on_drain),
      on_rebalance: Arc::new(on_rebalance),
    }
  }

  /// Replace the published status snapshot.
  pub fn set_status(&self, status: ClusterStatus) {
    *self.status.lock().expect("status") = status;
  }

  /// Router: `/cluster/status`, `/cluster/members`, `/cluster/processes`,
  /// `/cluster/drain`, `/cluster/rebalance`.
  pub fn router(self) -> Router {
    Router::new()
      .route("/cluster/status", get(status))
      .route("/cluster/members", get(members))
      .route("/cluster/processes", get(processes))
      .route("/cluster/drain", post(drain))
      .route("/cluster/rebalance", post(rebalance))
      .with_state(self)
  }
}

async fn status(State(ctl): State<ClusterControl>) -> Json<ClusterStatus> {
  Json(ctl.status.lock().expect("status").clone())
}

async fn members(State(ctl): State<ClusterControl>) -> Json<serde_json::Value> {
  let s = ctl.status.lock().expect("status");
  Json(serde_json::json!({ "members": s.members.clone() }))
}

async fn processes(State(ctl): State<ClusterControl>) -> Json<serde_json::Value> {
  let s = ctl.status.lock().expect("status");
  Json(serde_json::json!({ "processes": s.processes.clone() }))
}

async fn drain(
  State(ctl): State<ClusterControl>,
  Json(req): Json<DrainRequest>,
) -> Json<serde_json::Value> {
  {
    let mut s = ctl.status.lock().expect("status");
    s.draining = true;
  }
  (ctl.on_drain)(req);
  Json(serde_json::json!({ "ok": true }))
}

async fn rebalance(
  State(ctl): State<ClusterControl>,
  Json(req): Json<RebalanceRequest>,
) -> Json<serde_json::Value> {
  (ctl.on_rebalance)(req);
  Json(serde_json::json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::body::Body;
  use axum::http::{Request, StatusCode};
  use id_effect_node::{MemberView, ProcessView};
  use tower::ServiceExt;

  #[tokio::test]
  async fn status_endpoint_returns_members() {
    let mut st = ClusterStatus::new();
    st.members.push(MemberView {
      name: "a@t".into(),
      incarnation: "1".into(),
      addr: None,
    });
    let app = ClusterControl::snapshot(st).router();
    let res = app
      .oneshot(
        Request::builder()
          .uri("/cluster/status")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }

  async fn json(app: Router, method: &str, uri: &str, body: Option<&str>) -> serde_json::Value {
    let req = Request::builder()
      .method(method)
      .uri(uri)
      .header("content-type", "application/json")
      .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
      .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{method} {uri}");
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
      .await
      .unwrap();
    serde_json::from_slice(&bytes).unwrap()
  }

  fn status_with_rows() -> ClusterStatus {
    let mut st = ClusterStatus::new();
    st.members.push(MemberView {
      name: "a@t".into(),
      incarnation: "1".into(),
      addr: Some("127.0.0.1:1".into()),
    });
    st.processes.push(ProcessView {
      pid: "<a@t.1.1>".into(),
      node: "a@t".into(),
      names: vec!["worker".into()],
    });
    st
  }

  #[tokio::test]
  async fn members_and_processes_endpoints_list_rows() {
    let ctl = ClusterControl::snapshot(status_with_rows());
    let members = json(ctl.clone().router(), "GET", "/cluster/members", None).await;
    assert_eq!(members["members"][0]["name"], "a@t");
    assert_eq!(members["members"][0]["addr"], "127.0.0.1:1");
    let procs = json(ctl.router(), "GET", "/cluster/processes", None).await;
    assert_eq!(procs["processes"][0]["names"][0], "worker");
  }

  #[tokio::test]
  async fn set_status_replaces_the_published_snapshot() {
    let ctl = ClusterControl::snapshot(ClusterStatus::new());
    let before = json(ctl.clone().router(), "GET", "/cluster/status", None).await;
    assert_eq!(before["members"].as_array().map(Vec::len), Some(0));
    ctl.set_status(status_with_rows());
    let after = json(ctl.router(), "GET", "/cluster/status", None).await;
    assert_eq!(after["members"][0]["incarnation"], "1");
  }

  #[tokio::test]
  async fn drain_marks_draining_and_calls_the_hook() {
    let drained = Arc::new(Mutex::new(None));
    let rebalanced = Arc::new(Mutex::new(None));
    let (d, r) = (Arc::clone(&drained), Arc::clone(&rebalanced));
    let ctl = ClusterControl::with_hooks(
      ClusterStatus::new(),
      move |req| *d.lock().unwrap() = Some(req.grace_secs),
      move |req| *r.lock().unwrap() = Some(req.target_node),
    );
    let ok = json(
      ctl.clone().router(),
      "POST",
      "/cluster/drain",
      Some(r#"{"grace_secs":7}"#),
    )
    .await;
    assert_eq!(ok["ok"], true);
    assert_eq!(*drained.lock().unwrap(), Some(7));
    let st = json(ctl.clone().router(), "GET", "/cluster/status", None).await;
    assert_eq!(st["draining"], true);

    let ok = json(
      ctl.router(),
      "POST",
      "/cluster/rebalance",
      Some(r#"{"target_node":"b@t"}"#),
    )
    .await;
    assert_eq!(ok["ok"], true);
    assert_eq!(*rebalanced.lock().unwrap(), Some(Some("b@t".to_string())));
  }

  #[tokio::test]
  async fn snapshot_admin_actions_are_noops_that_still_ack() {
    let ctl = ClusterControl::snapshot(ClusterStatus::new());
    let ok = json(
      ctl.clone().router(),
      "POST",
      "/cluster/rebalance",
      Some(r#"{"target_node":null}"#),
    )
    .await;
    assert_eq!(ok["ok"], true);
    let ok = json(
      ctl.router(),
      "POST",
      "/cluster/drain",
      Some(r#"{"grace_secs":0}"#),
    )
    .await;
    assert_eq!(ok["ok"], true);
  }
}
