# HTTP via reqwest (`id_effect_platform::http::reqwest`)

The **`id_effect_platform::http::reqwest`** module integrates **`reqwest::Client`** with the
effect environment: the client lives in **`R`** as the **`ReqwestClient`** capability, and HTTP work
is expressed as **`Effect`** values (`send`, `text`, `bytes`, `json`, …).

## Relation to portable HTTP

| Use | Module |
|-----|--------|
| Portable **`HttpClient`** trait, swap implementations in tests, minimal request/response model | [`id_effect_platform::http`](./ch07-06-platform-services.md) (`HttpClientService`, `execute`, …) |
| Rich **reqwest** surface (`RequestBuilder`, redirects, pools), **JSON + `Schema`** decoding | **`id_effect_platform::http::reqwest`** |

Prefer **`id_effect_platform::http`** for new application boundaries that should stay
stack-agnostic. Use **`id_effect_platform::http::reqwest`** when you depend on **`RequestBuilder`**
pipelines, need **pooled clients** (`provide_reqwest_pool`, `send_pooled`), or want
**`json_schema`** so parse failures carry **field paths** (`id_effect::schema::ParseError`).

## Core pieces

- **`ReqwestClient(pub reqwest::Client)`** — the capability; `ReqwestClientEnv` is its
  one-element list.
- **`ReqwestClientLive::layer()`** / **`layer_with(config)`** — layers that build the client at
  the composition root from an `HttpClientConfig` (timeouts, no redirects, the SSRF resolver
  check; see [client defaults](./ch07-06-platform-services.md#the-ssrf-guard)).
  **`dangerous_provide_reqwest_client(client)`** supplies your own client unguarded, and records
  the opt-out.
- **`send`**, **`text`**, **`bytes`**, **`json`**, **`json_schema`** — effects over a
  `FnOnce(&Client) -> RequestBuilder`; `get` / `post` / `put` / `delete` / `head` / `patch` take a URL.
- **Pools** — `provide_reqwest_pool(n, ttl)` provides a `ReqwestPool` (`id_effect::Pool` of
  clients); `send_pooled` borrows one per request.

```rust,no_run
use id_effect::Exit;
use id_effect_platform::http::reqwest::{ReqwestClientEnv, ReqwestClientLive, text};
use id_effect_tokio::TokioRuntime;
use reqwest::Error;
# // Needs the network, so it is compiled but not run.
# async fn fetch(url: &'static str) -> Result<String, std::io::Error> {

let program = text::<String, Error, ReqwestClientEnv, _>(move |c| c.get(url))
    .provide_layer(ReqwestClientLive::layer());
let Exit::Success(body) = TokioRuntime::current()?.run(program).await else {
    panic!("request failed")
};
# Ok(body)
# }
```

Import reqwest types (`Client`, `Error`, …) from the **`reqwest`** crate and the helpers from
**`id_effect_platform::http::reqwest`**. Like every live platform effect these need a Tokio runtime
with its IO driver ([Tokio bridge](./ch07-05-tokio-bridge.md)).

## Further reading

- Examples: `030_layer_default_env`, `010_wiremock_get_text`, `020_wiremock_json` in
  `crates/id_effect_platform/examples`
- Crate-level docs: `cargo doc --open -p id_effect_platform`
- [Migrating from async](../appendix-b-migration.md) for the general `async fn` → `Effect` move.
