//! Axum + TestSqlClient integration sketch.
//!
//! ```bash
//! cargo run -p id_effect_sql --example 010_axum_sql
//! ```

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use id_effect::run_blocking;
use id_effect_sql::{SqlClient, SqlParam, TestSqlClient};

#[derive(Clone)]
struct AppState {
  sql: Arc<dyn SqlClient>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
  let client = TestSqlClient::default();
  run_blocking(client.connect(), ()).expect("connect");
  let state = AppState {
    sql: Arc::new(client),
  };

  let app = Router::new()
    .route(
      "/users/count",
      get(
        |axum::extract::State(st): axum::extract::State<AppState>| async move {
          let sql = Arc::clone(&st.sql);
          let n = run_blocking(
            sql.query("SELECT COUNT(*) FROM users", &[] as &[SqlParam]),
            (),
          )
          .map(|rows| rows.len())
          .unwrap_or(0);
          n.to_string()
        },
      ),
    )
    .with_state(state);

  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  println!(
    "SQL axum example: http://{}/users/count",
    listener.local_addr().unwrap()
  );
  axum::serve(listener, app).await.unwrap();
}
