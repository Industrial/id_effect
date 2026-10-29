---
title: '0.13 follow-ups (F62) — h3spec findings in the axum HTTP/3 adapter; http3-conformance joins ci-pass: r14-f-h3spec'
slug: r14-f-h3spec
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F62, adapter part (crates/id_effect_axum/src/server/h3.rs): the first CI run (h3spec v0.1.14, 77 cases, 19 failures; .maestro/missions/release-0-12.execution.md:183) attributes three groups to the adapter. (a) RFC 9114 §4.2: a request carrying `connection`, `keep-alive`, `proxy-connection`, `transfer-encoding`, `upgrade`, or `te` with any value but `trailers`, is malformed: `RequestTask::run` (:611) checks the resolved head (:627) before `serve_request` (:648) and resets the request stream with `H3_MESSAGE_ERROR` (send-side reset only, never `stop_sending`, F13 at :625); the router never sees it. (b) §4.1.1 / §4.1.3 duplicated or misplaced pseudo-headers, and (c) §7.1 a HEADERS frame over `SETTINGS_MAX_FIELD_SECTION_SIZE` (set at :158, `MAX_FIELD_SECTION_SIZE` :115): fixed in the adapter where the resolved head or a pre-resolve hook exposes them; where h3 0.0.8 (crates/id_effect_axum/Cargo.toml:148, exact pin) consumes them before the adapter sees anything, the case is reclassified upstream with the reason written down. Every one of the 19 cases ends in exactly one of: fixed here, or accepted upstream.'
  - 'F62, upstream part: each accepted case (h3 0.0.8: QPACK encoder/decoder stream errors, SETTINGS mid-parameter / repeated id, CANCEL_PUSH on a request stream, client push stream; quinn/rustls: `missing_extension` without quic_transport_parameters; plus any (b)/(c) case reclassified above) has a filed upstream issue or PR link (hyperium/h3, quinn-rs/quinn or rustls/rustls), listed in a new "### Conformance" subsection of the book chapter and in the comment above the h3spec step.'
  - 'CI: in .github/workflows/ci.yml job `http3-conformance` (:813) the h3spec step (:853, "report-only, F62") loses `continue-on-error` (:854) and runs `./h3spec 127.0.0.1 8443 --no-validate` with one hspec `--skip "<case>"` per accepted upstream case (each with its link); `http3-conformance` is added to `ci-pass.needs` (job at :959); the job is green on two consecutive runs on the PR before merge (the 0.13 rule for new required jobs, ci.yml:643).'
  - 'Tests: crates/id_effect_axum/tests/http3_conformance.rs (h3 client over a real QUIC endpoint, as tests/http3.rs does) `h3_rejects_connection_specific_headers_as_malformed` (one request per header, each a stream reset `H3_MESSAGE_ERROR`, the next request on the connection still served), `h3_allows_te_trailers`, `h3_malformed_request_never_reaches_the_router` (a counting middleware stays at 0), `h3_header_section_over_the_cap_is_refused`, plus one test per (b) case fixed here; unit test `connection_specific_headers_are_classified` in h3.rs; every existing test in crates/id_effect_axum/tests/http3.rs passes unchanged.'
  - 'Book: new subsection "### Conformance" under "## What clients see" in `crates/id_effect/book/src/part6/ch29-05-http3.md` (:68): what h3spec checks in CI, the cases fixed in 0.14 and the accepted upstream ones with links; example 092_http3 unchanged (it is what CI serves).'
  - 'Tier: integration (id_effect_axum). Secure by default (ADR 0013): R4 bounded input: malformed request heads are refused before the router and before any body is read; no new opt-out; HTTP/3 stays behind the `http3` feature with TCP always on.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_axum --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_axum --features http3 --test http3 --test http3_conformance`; `cargo clippy -p id_effect_axum --all-targets --all-features -- -D warnings`; CI job `http3-conformance` green twice with h3spec as a hard step, and `ci-pass` listing it'
  - 'Owns: `crates/id_effect_axum/src/server/h3.rs` (request-head validation in `RequestTask::run` and its helpers, `h3_builder` if the field-section fix needs it), `crates/id_effect_axum/tests/http3_conformance.rs` (new), `.github/workflows/ci.yml` (job `http3-conformance`; the `http3-conformance` line of `ci-pass.needs`, append-only), `crates/id_effect/book/src/part6/ch29-05-http3.md` (the new subsection); append-only: `CHANGELOG.md`'
---

# r14-f-h3spec

Leaf of `.maestro/specs/releases/0.14.0.md`, section "0.13 follow-ups". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: the wave-2 merge (r14-s-web-uploads edits id_effect_axum's `lib.rs` in wave 2,
never `server/h3.rs`). Blocks r14-x-kpis (the http3 KPI reads a required job). Other wave-3 leaves that
add CI jobs (r14-s-chaos, r14-s-deploy-k8s, r14-w-warden-core) append their own `ci-pass.needs` lines
and never edit job `http3-conformance`.

Defaults the owner may overturn: no h3 / h3-quinn / quinn version bump in this leaf (the pins are shared
with crates/id_effect_platform/Cargo.toml:97-98 and h3-webtransport; a bump is its own leaf); accepted
upstream cases are skipped by name in CI rather than the job staying report-only; the coordinator
records F62 closed in the release spec's "0.13 follow-ups" table (this leaf does not edit the release
spec). Not done here: patching or forking h3.
