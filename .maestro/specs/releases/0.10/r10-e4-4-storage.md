---
title: 'E4.4 — Storage, `id_effect_storage`: r10-e4-4-storage'
slug: r10-e4-4-storage
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`ObjectStore` capability over object_store (put/get/head/delete/list); streaming upload/download as `Stream<Bytes>` without buffering the object (#5); S3 backend (#3); presigned URLs for S3 and HMAC-signed expiring URLs for the local backend (#7); local FS and in-memory backends. `cargo deny check` green for object_store''s tree. Tests `stream_upload_never_buffers_whole_object`, `signed_url_expires`, `memory_and_local_conform`.'
  - 'Gate: `cargo nextest run -p id_effect_storage --all-features`; `cargo deny check`'
  - 'Owns: `crates/id_effect_storage/src/{lib.rs,store.rs,local.rs,memory.rs,s3.rs,signed.rs}`, `crates/id_effect_storage/Cargo.toml`'
---

# r10-e4-4-storage

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E4.4 — Storage, `id_effect_storage` (M, wave 1) · web.storage #3 #5 #7 (+#1 #2 #8 kept)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
