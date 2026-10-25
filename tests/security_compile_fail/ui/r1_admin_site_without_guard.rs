// ADR 0013 R1, R7 (0.13 breadth, admin): there is no public admin. An `AdminSite` mounts only
// after `.guard(g)` (the H8 typestate): an `AdminSite<Unguarded>` has no `router`, and the
// route-level `Public` marker is not a guard, so a site cannot be declared public either.
use std::sync::Arc;

use id_effect_admin::{AdminDatabase, AdminSite};
use id_effect_kit::axum::route::Public;
use id_effect_sql::{Dialect, TestSqlClient};

fn main() {
  let db = || AdminDatabase::new(Arc::new(TestSqlClient::new()), Dialect::Sqlite);
  let _ = AdminSite::new().router::<()>(db());
  let _ = AdminSite::new().guard(Public).router::<()>(db());
}
