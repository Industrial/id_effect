---
title: 'Proof — Fuzz targets for the new decoders, compile-fail security cases, the 0.13 security surface: r13-e-fuzz-security'
slug: r13-e-fuzz-security
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '≥ 5 new cargo-fuzz targets under fuzz/fuzz_targets/ (17 today → ≥ 22) with seed corpora: `i18n_accept_language` (`negotiate::parse_accept_language`), `sql_page_cursor` (`PageCursor::decode`), `mcp_jsonrpc_message` (`mcp::decode_message`), `ai_sse_event` (`stream::decode_sse_event`), `ai_tool_arguments` (`tools::decode_arguments`), `admin_form_body` (`forms::decode_form`); each builds and runs 5 minutes in ClusterFuzzLite PR mode with no crash.'
  - 'Compile-fail security cases in tests/security_compile_fail: `r1_admin_site_without_guard` (mounting `AdminSite` without `.guard`) and `r1_mcp_endpoint_without_access_decision` (the Streamable HTTP route without an access decision), each with a `help:` line.'
  - '`tests/security_compile_fail/tests/surface_013.rs` re-asserts that every test id in the release spec Security table exists in the tree (grep-based, as the 0.12 security_surface.rs) and runs the cross-crate cases; its PENDING list is empty at exit.'
  - 'THREAT-MODEL.md gains a "Breadth batteries" section: admin, server-rendered UI, tenancy, AI tools and prompt injection (with the residual risk), MCP, i18n input, each threat mapped to its test; docs/security/fuzzing.md lists the new targets.'
  - 'Tier: example (security_compile_fail, fuzz) and docs. Secure by default: this leaf is the mechanical check that every other leaf''s rule exists.'
  - 'Gate: `cargo +nightly-<pinned> fuzz build` (in fuzz/, toolchain from fuzz/rust-toolchain.toml); `cargo nextest run -p security_compile_fail`; `TMPDIR=/data/... cargo fuzz run <target> -- -max_total_time=300` for each new target'
  - 'Owns: `fuzz/**`, `tests/security_compile_fail/**`, `THREAT-MODEL.md` (breadth section), `docs/security/fuzzing.md`'
---

# r13-e-fuzz-security

Leaf of `.maestro/specs/releases/0.13.0.md`, section "Proof". The release spec's row is binding; this
file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: r13-e9-1-mcp, r13-e9-1-tools, r13-e9-4-i18n-web, r13-e3-3-pagination,
r13-e9-3-admin-edit (each exposes its decoder). Feeds r13-w8-kpis.
