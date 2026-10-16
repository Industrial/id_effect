# Doctor and the layer graph

Two tools answer "why does this project not start here?": `id-effect doctor` checks the machine and the project's configuration, and `id_effect_kit::graph::LayerGraph` shows which layer provides what to whom.

## `id-effect doctor`

```text
id-effect doctor [--path DIR] [--json] [--graph[=text|dot]]
```

`id-effect` is the binary of the `id_effect_tool` crate (`cargo install id_effect_tool`). Doctor runs every check on the project in `DIR` (default: the current directory) and prints one line per check: `pass`, `warn`, `fail` or `skip` (not applicable to this project), with the reason. It exits `1` when a check fails and `0` otherwise; warnings and skips do not change the exit status.

| Check | Fails when | Warns when | Skips when |
|---|---|---|---|
| `project` | `DIR` has no `Cargo.toml` (nothing else runs then) | | |
| `rustc` | `rustc --version` is older than the project's `rust-version` or the id_effect MSRV, or does not run | its output has no version | |
| `versions` | the id_effect crates in `Cargo.lock` resolve to more than one version | there is no `Cargo.lock`, or no id_effect crate in it | |
| `env` | a key of `.env.example` is set neither in the environment nor in `.env` | | there is no `.env.example` |
| `database` | `DATABASE_URL` is malformed, or its host does not accept a TCP connection | | `DATABASE_URL` is unset, not a TCP database, or only in the environment while `.env.example` does not list it |
| `docker` | | a `Dockerfile` or compose file is present but `docker` / `docker compose` is not | there is neither |
| `port` | `PORT` is not a port number, or `HOST:PORT` is in use | binding fails for another reason | there is no `PORT` |
| `graph` (`--graph`) | `cargo build` or the binary fails | the binary prints no graph, or one with missing capabilities | |

What doctor does not claim:

- `rustc` runs in the project directory, so a `rust-toolchain.toml` applies; `$RUSTC` overrides the compiler.
- `versions` reads the nearest `Cargo.lock` (the project's or its workspace's). Two versions of an id_effect crate put two copies in the build, and their types (`Effect`, `Layer`, capabilities) do not unify across copies, so this fails rather than warns.
- `env` and `database` read the process environment, then `.env`; values are never printed (`database` shows only `host:port`). `database` checks that the host accepts a TCP connection, not that the credentials work.
- `docker` checks the CLIs, not whether the daemon runs.
- `port` binds `HOST:PORT` (from the environment, `.env`, else `.env.example`; `HOST` defaults to `0.0.0.0` like `HostConfig`) and releases it at once. A running instance of the service makes it fail.

A freshly generated service, after `cp .env.example .env`:

```text
$ id-effect doctor
id-effect doctor: /home/me/orders-api
  pass  project   package `orders-api`
  pass  rustc     rustc 1.95.0 >= id_effect MSRV 1.94; the project sets no rust-version
  pass  versions  id_effect 0.8.0: id_effect, id_effect_axum, id_effect_cli, id_effect_config, id_effect_kit, …
  pass  env       all 5 .env.example keys set (5 from .env)
  skip  database  DATABASE_URL is not set (environment or .env)
  pass  docker    Docker version 29.8.1; Docker Compose version 5.6.0 (daemon not checked)
  pass  port      127.0.0.1:8080 (PORT from .env) is free

6 passed, 0 warnings, 0 failed, 1 skipped
```

A project whose lock mixes versions:

```text
  fail  versions  id_effect crates resolve to 2 versions: 0.6.2 (id_effect, id_effect_axum), 0.7.0 (id_effect); id_effect in the build twice, whose types do not unify across copies. Depend on one version and run `cargo update`
```

`--json` prints the same report as one object for scripts and CI:

```json
{"path":"/home/me/orders-api","ok":true,
 "counts":{"pass":6,"warn":0,"fail":0,"skip":1},
 "checks":[{"name":"project","status":"pass","reason":"package `orders-api`"}, …],
 "graph":null}
```

## The layer graph

A `Layer` keeps only its build function, so composing layers with `merge` or `provide_to` records no graph. Instead, the app registers each layer it composes in a `LayerGraph`, with a name. The graph reads each layer's capability lists from its type through the `TypeIds` trait (`Out` is what it provides, `In` what it needs), matches every need to the other registered layers providing it, and reports the needs nobody provides as missing.

```rust
use id_effect_kit::graph::{GraphFormat, LayerGraph};
use id_effect_kit::id_effect::{HCons, HNil};
use id_effect_kit::prelude::*;

struct Config;
struct Db;
struct Smtp;
struct Mailer;

let config: Layer<HCons<Config, HNil>, ()> = Layer::succeed(hlist![Config]);
let db: Layer<HCons<Db, HNil>, (), HCons<Config, HNil>> =
  Layer::effect(|| need::<Config>().map(|_| hlist![Db]));
let mailer: Layer<HCons<Mailer, HNil>, (), HCons<Config, HCons<Smtp, HNil>>> =
  Layer::effect(|| need::<Config>().map(|_| hlist![Mailer]));

let graph = LayerGraph::new()
  .with("config", &config)
  .with("db", &db)
  .with("mailer", &mailer);

assert_eq!(graph.edges().len(), 2);
assert_eq!(graph.missing()[0].capability.short_name(), "Smtp");
print!("{}", graph.render(GraphFormat::Text));
```

prints

```text
layer graph: 3 layers, 2 edges, 1 missing
config
  provides Config
db
  provides Db
  needs    Config <- config
mailer
  provides Mailer
  needs    Config <- config
  needs    Smtp <- MISSING (no registered layer provides it)
```

`GraphFormat::Dot` renders Graphviz (`dot -Tsvg`): a box per layer, an edge from provider to consumer labelled with the capability, and a dashed red node per missing capability. A missing capability is not necessarily a bug (the app may supply it with `provide_all`, or a test environment does), but it is where to look first.

### `--describe-layers` and `doctor --graph`

`LayerGraph::describe_if_requested(std::env::args())` returns the rendered graph when the arguments hold `--describe-layers` (text) or `--describe-layers=dot`, so `main` can print it and exit before it starts anything. The `service` template does this first thing in `main`:

<!-- ignore: an excerpt of the generated service's main.rs (crates/id_effect_tool/templates/service/src/main.rs.template) -->
```rust,ignore
let env = provide_host_config_env::<HostError>();
let host_config = HostConfig::layer::<HostError>();
let graph = LayerGraph::new()
    .with("env", &env)
    .with("host-config", &host_config);
if let Some(description) = graph.describe_if_requested(std::env::args()) {
    print!("{description}");
    return ExitCode::SUCCESS;
}
```

`id-effect doctor --graph` builds the project, runs `cargo run -- --describe-layers` (stopping the binary after 60 seconds if it ignores the flag and starts the app instead), adds a `graph` check from the summary line and prints the graph below the report:

```text
  pass  graph     2 layers, 1 edges, 0 missing

layer graph: 2 layers, 1 edges, 0 missing
env
  provides ConfigProviderService
host-config
  provides HostConfig
  needs    ConfigProviderService <- env
```

Add each layer you compose to the graph as the app grows, and the picture stays current.
