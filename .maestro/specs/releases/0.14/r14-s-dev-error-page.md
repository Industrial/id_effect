---
title: 'E13.1 Sweep — dev-profile error page with source context for axum handler defects: r14-s-dev-error-page'
slug: r14-s-dev-error-page
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'dx.diagnostics#8 (row dx.diagnostics 75 → 88; #6 IDE plugin stays open, 0.15): a dev-mode error page with source context. crates/id_effect_axum, feature `dev-errors` (with `ui`, maud, already a dependency through `ui = ["dep:maud"]`): `DevErrorPage::new(source_root)` (an app passes `env!("CARGO_MANIFEST_DIR")`) is a tower layer that, in the `dev` profile only, replaces the opaque `500` body of a handler defect (crates/id_effect_axum/src/lib.rs:230-241 `cause_response`: body "internal server error" plus the `HandlerDefect` extension, :195-201) with an HTML page: the defect message, the method, path and request id, the cause''s trace, and for each trace frame inside `source_root` the source lines around it (±7, the failing line marked). A client that does not accept `text/html` gets `application/problem+json` with the same fields instead.'
  - 'Where the frames come from: `cause_response` also inserts a new `HandlerDefectTrace` response extension (the rendered trace and its file:line frames) next to `HandlerDefect`, built from the span / stack traces that r14-s-core-errors (wave 0, core.cause#7) attaches to `Cause`. `HandlerDefect` itself keeps its one public field (no semver break); the new extension is a separate `#[non_exhaustive]` type.'
  - 'Secure by default (ADR 0013 R9, Security table row "Dev error page"): the profile is `DevErrorPage::profile(..)`, default the `ID_EFFECT_PROFILE` variable, decided as `OpenApiUi` decides its open profiles (crates/id_effect_axum/src/openapi/ui.rs:92-97); in every profile but `dev` (`test` and `prod` included) the layer is a pass-through and the response is byte-for-byte the normal opaque `500`. No API enables it in another profile (so there is no `dangerous_` opt-out to record). Source is read only from regular files whose canonical path is under the canonical `source_root` (no symlink or `..` escape), at most 1 MiB per file and 16 frames per page; every value is HTML-escaped by maud; the extensions never reach the response body outside the dev page.'
  - 'Tests: crates/id_effect_axum/tests/dev_errors.rs `dev_profile_renders_defect_with_source_context`, `dev_page_marks_the_failing_line`, `non_html_client_gets_problem_json_in_dev`, `prod_profile_response_is_the_opaque_500`, `test_profile_response_is_the_opaque_500`, `source_outside_root_is_never_read`, `symlink_escape_is_never_read`, `typed_failures_and_non_defect_responses_pass_through`, `defect_message_html_is_escaped`; unit tests of `HandlerDefectTrace` in lib.rs.'
  - 'Book: new chapter crates/id_effect/book/src/part6/ch34-04-dev-error-page.md (what the page shows, why it is dev-only, how to wire it), compiled by examples/book-snippets/src/part6.rs. Example crates/id_effect_axum/examples/096_dev_error_page.rs (a route that panics; run with `ID_EFFECT_PROFILE=dev`).'
  - 'Tier: integration (id_effect_axum, unchanged). No new dependency.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_axum --all-features --summary-only --fail-under-regions 96`'
  - 'Gate: `cargo nextest run -p id_effect_axum --features dev-errors --test dev_errors`; `cargo nextest run -p id_effect_axum --all-features`; `cargo clippy -p id_effect_axum --all-targets --all-features -- -D warnings`; `cargo test -p book_snippets`; `bash scripts/check-dangerous.sh`'
  - 'Owns: `crates/id_effect_axum/src/middleware/dev_errors.rs` (new), `crates/id_effect_axum/src/middleware/mod.rs` (the `dev_errors` module line; append-only), `crates/id_effect_axum/src/lib.rs` (new type `HandlerDefectTrace` beside `HandlerDefect` :195-201, its insertion in `cause_response` :230-241, the re-export and a crate-doc line), `crates/id_effect_axum/Cargo.toml` (feature `dev-errors` and the `[[example]] 096_dev_error_page` entry), `crates/id_effect_axum/tests/dev_errors.rs` (new), `crates/id_effect_axum/examples/096_dev_error_page.rs` (new), `crates/id_effect/book/src/part6/ch34-04-dev-error-page.md` (new), append-only: `crates/id_effect/book/src/SUMMARY.md`, `examples/book-snippets/src/part6.rs` (include line), `examples/book-snippets/Cargo.toml` (`dev-errors` on the id_effect_axum line), `CHANGELOG.md`'
---

# r14-s-dev-error-page

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task. Split from r14-s-dx-docs-lint
(dx.diagnostics#8 only).

Wave 3. Blocked by: r14-s-core-errors (wave 0, the traces on `Cause`). Blocks r14-p-toolchain (wave 5
promotes `dx.diagnostics`; it also adds a kit feature and the `service` template wiring, which this
leaf leaves alone so it shares no file with r14-s-chaos, which edits crates/id_effect_kit/Cargo.toml in
the same wave). Same-wave neighbour r14-f-h3spec owns crates/id_effect_axum/src/server/h3.rs only.

Defaults the owner may overturn: (1) only handler defects (`Cause::Die`) get the page; a typed failure
mapped to a 5xx by the app's own `IntoResponse` is the app's response and passes through. (2) If
r14-s-core-errors ships traces without file:line frames, the page shows the trace text and the request
without source lines, and the source-context criterion is reported back to the coordinator rather than
the leaf reaching into core. (3) No loopback-only restriction: the profile gate is the control, as for
the OpenAPI UI.
