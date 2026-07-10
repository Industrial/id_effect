# HTTP via reqwest (`id_effect_platform::http::reqwest`)

The **`id_effect_platform::http::reqwest`** module integrates **`reqwest::Client`** with the effect environment: the client lives in **`R`** behind **`ReqwestClient`**, and HTTP work is expressed as **`Effect`** values (`send`, `text`, `bytes`, `json`, …).

## Relation to portable HTTP

| Use | Module |
|-----|--------|
| Portable **`HttpClient`** trait, swap implementations in tests, minimal request/response model | [`id_effect_platform::http`](./ch07-06-platform-services.md) (`HttpClientService`, `execute`, …) |
| Rich **reqwest** surface (`RequestBuilder`, redirects, pools), **JSON + `Schema`** decoding | **`id_effect_platform::http::reqwest`** |

Prefer **`id_effect_platform::http`** for new application boundaries that should stay stack-agnostic. Use **`id_effect_platform::http::reqwest`** when you depend on **`RequestBuilder`** pipelines, need **pooled clients** (`provide_reqwest_pool`, `send_pooled`), or want **`json_schema`** so parse failures carry **field paths** (`id_effect::schema::ParseError`).

Import reqwest types (`Client`, `Error`, …) from the **`reqwest`** crate; import helpers from **`id_effect_platform::http::reqwest`**. Drive async steps with **`id_effect::run_async`** per [Tokio bridge](./ch07-05-tokio-bridge.md).

## Core pieces

- **`ReqwestClient`** — tag for the active `reqwest::Client` in `R`.
- **`provide_reqwest_client` / `ReqwestClientLive`** — construct the client at the composition root.
- **`send`**, **`text`**, **`bytes`**, **`json`**, **`json_schema`** — effectful helpers over `RequestBuilder`.
- **Optional pools** — TTL pools of clients for connection churn scenarios.

## Further reading

- Crate-level docs: `cargo doc --open -p id_effect_platform`
- [Migrating from async](../appendix-b-migration.md) for the general `async fn` → `Effect` move; combine with this module at HTTP boundaries when you choose the reqwest adapter.
