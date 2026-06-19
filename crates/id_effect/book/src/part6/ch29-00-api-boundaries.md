# API boundaries

[`id_effect_rpc`](../../../id_effect_rpc) implements Phase D RPC-shaped HTTP boundaries: correlation ids, JSON error envelopes, OpenAPI emission, codegen stubs, API versioning, and tracing helpers.

## Correlation and errors

Use [`ensure_correlation_id`](https://docs.rs/id_effect_rpc/latest/id_effect_rpc/correlation/fn.ensure_correlation_id.html) and [`RpcError`](https://docs.rs/id_effect_rpc/latest/id_effect_rpc/struct.RpcError.html) for consistent wire errors. Validate bodies with `id_effect_axum::json::decode_json_schema`.

## OpenAPI and codegen

- [`openapi::emit_openapi_json`](https://docs.rs/id_effect_rpc/latest/id_effect_rpc/openapi/index.html) — route metadata → OpenAPI 3
- [`codegen::emit_service_trait`](https://docs.rs/id_effect_rpc/latest/id_effect_rpc/codegen/index.html) — D3 trait stub generation

## API versioning

[`versioning::negotiate_api_version`](https://docs.rs/id_effect_rpc/latest/id_effect_rpc/versioning/fn.negotiate_api_version.html) resolves version from `Accept-Version` or `/vN/` path prefix:

```rust
use id_effect_rpc::versioning::{ApiVersion, VersionConfig, negotiate_api_version};

let cfg = VersionConfig::new(ApiVersion::new("v1"), vec![ApiVersion::new("v1")]);
let app = router.layer(axum::middleware::from_fn_with_state(cfg, negotiate_api_version));
```

## RPC tracing example

```bash
cargo run -p id_effect_rpc --example 020_rpc_tracing
```

Spans use stable field names consumable by `id_effect_opentelemetry` layers.

## See also

- Part II: [RPC boundaries](../part2/ch07-12-rpc-boundaries.md)
- Mission: `platform-api-boundaries`
