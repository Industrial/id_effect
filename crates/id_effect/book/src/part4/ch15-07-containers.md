# Containers in Tests

Fakes cover most tests, but some behaviour only exists in the real dependency: a PostgreSQL
constraint, a Redis expiry, a Keycloak login. `id_effect_test` starts those dependencies in
Docker (through [testcontainers](https://docs.rs/testcontainers)) and hands them to the test as
**effect resources**: a container is started when its layer is built and removed when the
layer's scope closes, whether the test succeeded, failed, panicked or was interrupted.

```toml
[dev-dependencies]
id_effect_test = { version = "0.9", features = ["sql"] } # `sql`: a ready SqlClient over Postgres
```

| Image | Running value (the capability) | Connection details |
|-------|-------------------------------|--------------------|
| `Postgres` (`postgres:17-alpine`) | `PostgresContainer` | `url()`, `url_for(db)`, `host()`, `port()`; `sql_layer()` with `sql` |
| `MySql` (`mysql:8.4`) | `MySqlContainer` | `url()`, `url_for(db)`, `root_url()` |
| `Redis` (`redis:7-alpine`) | `RedisContainer` | `url()` |
| `Keycloak` (`quay.io/keycloak/keycloak:26.4`) | `KeycloakContainer` | `base_url()`, `issuer(realm)`, `import_realm(json)` |
| `Vault` (`hashicorp/vault:1.20`, dev mode) | `VaultContainer` | `addr()`, `root_token()` |
| `ContainerSpec::new(image, tag)` | `Container` | `host()`, `host_port(p)`, `address(p)` |

Each image is a builder (`Postgres::new().tag("16-alpine").database("ledger")`) and implements
`ContainerImage`, whose provided methods make it a resource:

| Method | Started | Removed |
|--------|---------|---------|
| `image.layer::<E>()` | when the layer is built | when the build's scope closes |
| `image.acquire::<E, R>(&scope)` | now | when `scope` closes |
| `image.start::<E, R>()` | now | by `container.remove()`, or when the last clone is dropped |
| `Shared::new(make).layer::<E>()` | once per test binary | after the process exits (the reaper) |

`E` is any error type with `From<ContainerError>`.

## A container per test

`need::<RedisContainer>()` reads the running container like any other capability, and
`provide_layer(Redis::new().layer())` supplies it. The program runs after the container is ready;
the container is gone again when `provide_layer`'s scope closes.

```rust,no_run
use id_effect::{Effect, Exit, caps, need};
use id_effect_test::{ContainerError, ContainerImage, Redis, RedisContainer};

# /* compiled as a plain fn in the doctest
#[test]
# */
fn caches_in_redis() {
  if !id_effect_test::require_docker() {
    return; // prints `SKIP caches_in_redis: Docker is not available (…)`
  }
  let program: Effect<String, ContainerError, caps!(RedisContainer)> =
    need::<RedisContainer>().map(|redis| redis.url());
  let Exit::Success(url) = program.provide_layer(Redis::new().layer()).block_on() else {
    panic!("redis did not start")
  };
  assert!(url.starts_with("redis://"));
}
```

Removal does not depend on the program succeeding. `provide_layer` owns the scope of its build
and closes it after the effect ends with any `Exit`: a typed failure, a defect, or an interrupt
(a `timeout`, a lost `race`, a cancelled fiber). The start itself is uninterruptible — an
interrupt that arrives while a container is starting waits until it is ready (or its readiness
timeout fails it), then removes it — so a started container is always registered for removal.
The crate's own tests check all three exits by asking Docker afterwards
(`id_effect_test::container_exists(id)`).

Layers compose as usual: `Postgres::new().sql_layer().merge(Redis::new().layer())` builds both
containers for one program and removes both, in reverse order.

## PostgreSQL with a ready `SqlClient`

With the `sql` feature, `Postgres::sql_layer()` is the container plus `PgSqlClient::layer` over
its URL: it provides `PostgresContainer`, `SqlClientService` and the pool. When the scope closes
the pool is closed first, then the container is removed. sqlx needs tokio, so run it on
`TokioRuntime`.

```rust,no_run
# #[cfg(feature = "sql")]
# mod example {
use id_effect::{Effect, Exit, caps};
use id_effect_sql::{SqlClientService, SqlError, query};
use id_effect_test::{ContainerError, Postgres};
use id_effect_tokio::TokioRuntime;

#[derive(Debug)]
enum TestError {
  Container(ContainerError),
  Sql(SqlError),
}
impl From<ContainerError> for TestError {
  fn from(e: ContainerError) -> Self { Self::Container(e) }
}
impl From<SqlError> for TestError {
  fn from(e: SqlError) -> Self { Self::Sql(e) }
}

# /* compiled as a plain fn in the doctest
#[tokio::test]
# */
async fn counts_rows() {
  if !id_effect_test::require_docker() {
    return;
  }
  let one: Effect<_, TestError, caps!(SqlClientService)> =
    query("SELECT 1::int8 AS one", vec![]).map_err(TestError::from);
  let exit = TokioRuntime::current()
    .unwrap()
    .run(one.provide_layer(Postgres::new().sql_layer()))
    .await;
  let Exit::Success(rows) = exit else { panic!("{exit:?}") };
  assert_eq!(rows[0].get::<i64>("one"), Ok(1));
}
# }
# fn main() {}
```

The data directory is a tmpfs and `fsync` is off, so a fresh server is ready in about a second.

## One container per test binary

Starting PostgreSQL for every test is affordable; starting Keycloak (a JVM) is not. `Shared`
keeps one container in a `static`, started by the first test that asks for it and handed to
every later one:

```rust,no_run
use id_effect::{Effect, caps, need};
use id_effect_test::{ContainerError, Keycloak, KeycloakContainer, Shared};

static IDP: Shared<Keycloak> =
  Shared::new(|| Keycloak::new().import_realm(r#"{"realm": "ledger", "enabled": true}"#));

fn issuer() -> Effect<String, ContainerError> {
  let program: Effect<String, ContainerError, caps!(KeycloakContainer)> =
    need::<KeycloakContainer>().map(|kc| kc.issuer("ledger"));
  program.provide_layer(IDP.layer())
}
# let _ = issuer;
```

Closing a shared layer's scope does not remove the container, and statics are never dropped,
so the first shared start also starts a **reaper** (testcontainers' Ryuk): the test process
keeps a connection to it, and once the process exits — normally, by panic, or killed — Ryuk
removes every container that process started. A failed shared start is remembered: every later
test gets the same `ContainerError` at once instead of waiting for its own timeout.

Tests sharing a container share its state; isolate data per test (a schema or database per test,
a key prefix, a rolled-back transaction). Note that `cargo nextest` runs each test in its own
process, so there "one per binary" means one per test; `cargo test` shares it across the
binary's tests.

## Readiness and typed timeouts

A start returns only after the container is ready. Each image knows what ready means —
PostgreSQL and MySQL when their health command answers over TCP (after the init scripts), Redis
and Vault when they log it, Keycloak when it is listening — and every wait is bounded by a
readiness timeout (`ready_timeout(d)`, default 120 s, 180 s for Keycloak). The outcomes are typed:

| `ContainerError` | When |
|------------------|------|
| `DockerUnavailable { reason }` | the Docker daemon does not answer a ping |
| `Start { image, reason }` | Docker could not create or start it (unknown image, pull failure) |
| `NotReady { image, id, timeout }` | not ready within the timeout; the container was removed |
| `Unready { image, id, reason }` | it exited or turned unhealthy first; the container was removed |
| `PortNotExposed { image, port }` | `host_port(p)` for a port the spec did not publish |

## Any image

`ContainerSpec` describes any other container — environment, published ports, command,
readiness strategies (`Wait::stdout/stderr/log(msg)`, `Wait::healthy()` with `health_cmd`,
`Wait::delay(d)`), tmpfs mounts, labels, files copied in:

```rust
use std::time::Duration;
use id_effect_test::{ContainerSpec, Wait};

let mailpit = ContainerSpec::new("axllent/mailpit", "v1.21")
  .port(1025)
  .port(8025)
  .env("MP_SMTP_AUTH_ACCEPT_ANY", "1")
  .wait(Wait::log("accessible via"))
  .ready_timeout(Duration::from_secs(30));
assert_eq!(mailpit.image(), "axllent/mailpit:v1.21");
```

A spec is itself a `ContainerImage` whose running value is the plain `Container`. To give it a
capability of its own (so two generic containers can live in one environment), implement
`ContainerImage` for a small type: `spec()` returns the spec, `running(container)` wraps the
container in your newtype, `container(&running)` unwraps it.

## Docker in CI and locally

Docker is found as testcontainers finds it (`DOCKER_HOST`, `~/.testcontainers.properties`, the
default socket). Every container test starts with `if !id_effect_test::require_docker() { return; }`:

- Docker available: the test runs.
- No Docker: the test prints `SKIP <test>: Docker is not available (…)` and passes, so a laptop
  without Docker still runs the suite.
- No Docker and `ID_EFFECT_REQUIRE_DOCKER=1`: the test **fails**. CI's `containers-integration`
  job sets it, so a broken Docker setup cannot turn the container suite into silent skips.

The Docker calls themselves run on a small runtime owned by `id_effect_test`, so the container
effects run on any executor (`block_on`, `TokioRuntime`, a `#[tokio::test]` of either flavour);
only what talks to the containers (sqlx in `sql_layer`) needs the executor it needs.
