# `id_effect_test`

Integration-test support for id_effect programs.

- **Containers as effect resources** (testcontainers): `Postgres` (with feature `sql`, a ready
  `SqlClient` via `Postgres::sql_layer`), `MySql`, `Redis`, `Keycloak`, `Vault` and any image
  through `ContainerSpec`. `image.layer()` starts the container when the layer is built and
  removes it when the layer's scope closes — after success, failure or interrupt;
  `Shared::new(make)` keeps one container per test binary (removed by a reaper after the
  process exits).
- **Typed readiness**: every start waits for its readiness strategy under a timeout
  (`ContainerError::NotReady`); a container that is not ready is removed.
- **`require_docker()`**: without Docker a test prints `SKIP` and passes, unless
  `ID_EFFECT_REQUIRE_DOCKER=1` (CI), where it fails.

Per-test database isolation (`isolated_db()`, `TemplateDb`), seeded fixtures (`Factory`) and an
in-process HTTP `TestClient` are behind the features `sql`, `fixtures` and `http`.

```rust
use std::time::Duration;
use id_effect_test::{ContainerSpec, Wait};

// Any image: published ports, environment, readiness bounded by a timeout.
let echo = ContainerSpec::new("hashicorp/http-echo", "1.0")
  .cmd(["-text=hello"])
  .port(5678)
  .wait(Wait::stderr("server is listening"))
  .ready_timeout(Duration::from_secs(30));
assert_eq!(echo.image(), "hashicorp/http-echo:1.0");

// In a test: `if !id_effect_test::require_docker() { return; }`, then
// `program.provide_layer(echo.layer())` — see the book chapter "Containers in Tests".
```

Tier: `integration` ([ADR 0010](../../docs/adrs/0010-crate-tiers.md)).
