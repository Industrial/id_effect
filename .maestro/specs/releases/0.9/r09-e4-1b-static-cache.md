---
title: 'E4.1b — HTTP middleware set: r09-e4-1b-static-cache'
slug: r09-e4-1b-static-cache
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`serve_dir` with ETag / Last-Modified / 304 (web.server#7); `HttpCacheLayer` sets Cache-Control and answers `If-None-Match` for GET responses (data.cache#7). Tests `conditional_get_returns_304`, `etag_changes_with_body`. Book `ch29-03-middleware.md`, example `090_middleware.rs`.'
  - 'Gate: same + book-snippets'
  - 'Owns: `crates/id_effect_axum/src/server/static_files.rs`, `src/middleware/http_cache.rs`'
---

# r09-e4-1b-static-cache

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E4.1b — HTTP middleware set (M, wave 2) · web.middleware #4 #5 #8 (+#6), data.cache #7, web.server #7". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
