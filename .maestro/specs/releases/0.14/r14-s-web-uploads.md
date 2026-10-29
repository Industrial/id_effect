---
title: 'E13.1 Sweep — streaming multipart file uploads (axum) into object storage: r14-s-web-uploads'
slug: r14-s-web-uploads
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'web.storage#6: `id_effect_axum::upload` (new module; axum `multipart` is already on, crates/id_effect_axum/Cargo.toml:68) gives the `Uploads` extractor that yields file parts as `UploadedFile` (form field name, declared content type, sniffed content type, a display-only sanitised filename hint, and the body as an id_effect `Stream<Bytes, UploadError>` pulled chunk by chunk, never buffered whole) plus the text fields; limits come from an `UploadPolicy` built type-state style: `UploadPolicy::new().max_file_bytes(n).allow_types([..])` is the only way to get a policy the extractor accepts, so a route without a size cap or a content-type allow-list does not compile. Today only the text fields of a multipart form are read and file parts are skipped (crates/id_effect_axum/src/route/decode.rs:243-271). Row web.storage 75 → 88 (#4 GCS/Azure stays 0.15 E13.3).'
  - 'Storage side, feature `upload` of id_effect_storage (new optional dependency on id_effect_axum; id_effect_axum never depends on id_effect_storage, so no cycle): `store_upload(file, prefix)` streams an `UploadedFile` into the `ObjectStorage` capability through `put_stream` (crates/id_effect_storage/src/store.rs:800; S3 multipart beyond one part, as tests/s3.rs:187), under a server-generated key `<prefix>/<uuid v4>` (never from the client filename), returning `StoredUpload { key, size, sha256, content_type }`; on a cap or type failure mid-stream the partial object is deleted and the error returned.'
  - 'Tests: crates/id_effect_axum/tests/upload.rs `upload_parts_stream_in_bounded_chunks`, `upload_over_file_cap_is_413`, `upload_with_type_outside_allow_list_is_415`, `upload_whose_magic_bytes_contradict_its_type_is_415`, `upload_part_and_field_counts_are_capped`, `upload_filename_hint_is_sanitised`, `upload_route_keeps_csrf_check` (the header path of crates/id_effect_axum/src/server/security/csrf.rs:17); crates/id_effect_storage/tests/upload.rs `store_upload_streams_to_memory_and_local_stores`, `store_upload_key_never_derives_from_client_filename`, `failed_upload_leaves_no_object`, `store_upload_to_s3_is_multipart` (S3 container, containers-integration, ci.yml:347 already runs `-p id_effect_storage --all-features`).'
  - 'Book: new section "## Uploads from the web" in `crates/id_effect/book/src/part6/ch28-05-storage.md` (after "## Streaming without buffering", :59); example `crates/id_effect_storage/examples/030_uploads.rs` (required-features `upload`).'
  - 'Tier: integration (id_effect_axum), platform (id_effect_storage; Ledger consumer). Secure by default (ADR 0013): R4 bounded before buffering: per-file cap (required), whole-body cap through the route''s `DefaultBodyLimit` (crates/id_effect_axum/src/middleware/limits.rs:166) set to at least the policy total, at most `max_files` (default 8) parts and 64 KiB per text field; content types from an explicit allow-list, and for the image/PDF/zip types the first chunk''s magic bytes must agree with the declared type (a small built-in signature table, no new dependency); no path or key from the client filename. The compile-fail case and the fuzz target are r14-e-fuzz-security''s (`r4_upload_route_without_size_cap` and `multipart_upload` there; `r1_…` and `multipart_body` in the release spec''s Security table, whose R-numbers predate ADR 0013''s); this leaf makes the type-state the compile-fail case relies on and exposes the policy-enforcing part reader as a documented public fn for the target (`upload::read_parts(policy, content_type, body)`).'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_axum --all-features --summary-only --fail-under-regions 96`; `cargo llvm-cov nextest -p id_effect_storage --all-features --summary-only --fail-under-regions 96` (with Docker for the S3 test).'
  - 'Gate: `cargo nextest run -p id_effect_axum --all-features --test upload`; `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_storage --all-features`; `cargo clippy -p id_effect_axum -p id_effect_storage --all-targets --all-features -- -D warnings`; `bash scripts/tier-conformance.sh id_effect_storage`'
  - 'Owns: `crates/id_effect_axum/src/upload.rs` (new), `crates/id_effect_axum/tests/upload.rs` (new), `crates/id_effect_storage/src/upload.rs` (new), `crates/id_effect_storage/tests/upload.rs` (new), `crates/id_effect_storage/examples/030_uploads.rs` (new), `crates/id_effect_storage/Cargo.toml` (feature `upload`, the optional id_effect_axum, uuid and sha2 dependencies, `[[example]]` entry), `crates/id_effect/book/src/part6/ch28-05-storage.md` (the new section), the kit feature line `storage-upload` in `crates/id_effect_kit/Cargo.toml`; append-only: `crates/id_effect_axum/src/lib.rs` (the `pub mod upload;` line), `crates/id_effect_kit/src/lib.rs` (the `storage-upload` feature-table row), `crates/id_effect_storage/src/lib.rs` (mod and `pub use` lines), `CHANGELOG.md`, `Cargo.lock`'
---

# r14-s-web-uploads

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: the wave-1 merge (no wave-1 leaf owns its files). Blocks r14-e-fuzz-security
(`multipart_upload`, `r4_upload_route_without_size_cap`). The only wave-2 leaf that edits
crates/id_effect_axum (r14-s-authz-views stays in crates/id_effect_authz); r14-f-h3spec edits axum's
`server/h3.rs` in wave 3.

Defaults the owner may overturn: the extractor lives in id_effect_axum and the storage bridge in
id_effect_storage behind `upload` (the dependency points storage → axum); magic-byte sniffing only for
types with a fixed signature (text types are trusted as declared, then served with `nosniff`); no
typed-route (`route/decode.rs`) integration for file fields in this leaf: handlers take `Uploads`
directly. Not done here: resumable uploads (tus), virus scanning, image processing, direct-to-S3
browser uploads (signed URLs already cover that, web.storage#7).
