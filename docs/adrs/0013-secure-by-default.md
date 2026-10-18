# ADR 0013 — Secure by default

## Status

Proposed (2026-10-18). Release 0.11.0, epic E11.2, leaf `r11-sec-adr0013`. The owner moves it
to Accepted ([0.11.0 D22](../../.maestro/specs/releases/0.11.0.md)); the 0.11.0 exit KPI needs
"Accepted" and every rule's enforcement point green at the tag. The rules, findings and leaves
come from the release spec's table "ADR 0013 'Secure by default' — the ten rules" and its
"Findings → leaves" table; the findings are those of the 2026-10-18 security review.

## Context

The 2026-10-18 review of the workspace found one open high and twenty medium and low findings
after the 0.10.1 fixes (H3–H7, M2, M9, M15, M20, L4). They share one shape: the safe behaviour
exists, but it is opt-in, and the default is the unsafe one.

- **Access is open unless guarded (H8, M19).** A route without a guard serves anonymous callers;
  tenant isolation is a policy you have to remember to add.
- **Secrets leak by default (M13, M14, L3).** Secrets are never zeroized, a derived `Debug` prints
  session attributes, and `Secret` equality is not constant time.
- **Logs are raw (M11, M12).** `RedactionPolicy::new()` redacts nothing, and messages and keys are
  written unescaped, so input can forge lines and fields.
- **Untrusted input is buffered unbounded (M4, M5, M8, M10).** The template CSRF layer reads 2 MiB
  outside the limit layers, there is no concurrency cap, rate-limit eviction scans every key under
  one mutex, and process mailboxes are unbounded and remote-reachable.
- **Raw SQL and destructive calls are one typo away (M16, M17).** `query`/`execute` take any
  `String`, and Redis `clear()` with an empty namespace deletes every key.
- **Outbound requests are unguarded (M3).** No SSRF guard, and redirects are followed.
- **Operator surfaces are open (M6, M7, L2).** The template console trusts loopback, `/docs` loads
  an unpinned bundle without SRI, and `/metrics` is on the public router.
- **Browser defences are partial (L1, L2, L5).** The CSRF token is static and not rotated at
  login; COOP, CORP and Permissions-Policy are missing.
- **Shared security state is per-process (M18).** The TOTP replay store is memory-only.

Fixing each finding where it is would leave the next battery free to repeat the pattern. 0.11.0
commits the §6 KPI "0 open highs; ADR 0013 accepted with an enforcement point per rule", so the
fix has to be a rule set that the build checks, not a review habit.

## Decision

### Definition of a closed finding

A finding is **closed** when the default path is safe and the old behaviour survives only as a
named `dangerous_*`/`insecure_*` opt-out (or an L9 deprecated shim) that is recorded by the
posture registry and fails the prod posture check. Each closed finding has a regression test
named after it (`h8_…`, `m3_…`, `l1_…`). A finding that is not closed is either open or recorded
as an accepted risk in `THREAT-MODEL.md` with a reason.

### The ten rules

Each rule has at least one mechanical enforcement point: a type or compile-fail test, a default
plus a regression test, a prod posture probe, or a CI gate. The rule holds while its enforcement
points are green.

| Rule | Statement | Findings | Enforcement point (mechanical) | Leaves |
|---|---|---|---|---|
| R1 | Deny by default: every route declares public or guarded access; resources scoped to a tenant require the tenant match. | H8, M19 | compile-fail `h8_route_without_access_decision`; runtime `h8_unregistered_axum_route_is_denied`; `m19_tenanted_resource_requires_tenant_match_by_default` | r11-sec-h8-routing, r11-sec-tenancy |
| R2 | Secrets are typed, redacted in every `Debug`/`Display`, compared in constant time and zeroized on drop. | M13, M14, L3 | `m13_*`, `m14_*`, `l3_*` tests; compile-fail on generic `Secret` equality | r11-sec-secrets |
| R3 | Logs are redacted and escaped by default. | M11, M12 | `m11_default_pipeline_redacts_common_secret_fields`, `m12_*`; posture probe "redaction policy non-empty" | r11-sec-logging |
| R4 | Untrusted input is bounded before it is buffered: bodies, forms, mailboxes, rate-limit state, in-flight requests. | M4, M5, M8, M10 | `m4_*`, `m5_*`, `m8_*`, `m10_*` tests; posture probe "load shedding on" | r11-sec-csrf-headers, r11-sec-limits, r11-e8-1-mailbox, r11-sec-ops-surfaces |
| R5 | Raw SQL needs a capability; destructive operations cannot target everything by accident. | M16, M17 | compile-fail `m16_query_with_string`; lint `raw_sql_non_literal`; `m17_clear_with_empty_namespace_is_an_error` | r11-sec-sql-redis, r11-e1-9-dylint |
| R6 | Outbound requests are guarded: SSRF ranges denied, no implicit redirects, timeouts and body caps on. | M3 (M2 in 0.10.1) | `m3_*` tests; posture probe "SSRF guard on" | r11-sec-client-ssrf |
| R7 | Operator surfaces (console, /docs, /metrics) are closed in prod unless guarded. | M6, M7, L2 | `m6_*`, `m7_*`, `l2_template_metrics_not_public`; posture probes "console guarded", "docs closed" | r11-sec-ops-surfaces, r11-w3-ledger |
| R8 | Transports are encrypted and mutually authenticated by default; plaintext needs a feature and a named opt-out. | H3/H4 follow-up, dist.tls#2 #4 | `mtls_rejects_peer_without_client_cert`; compile-fail "plaintext TCP without the feature"; posture probe "mesh transport encrypted" | r11-e8-3-quic-mtls, r11-e8-3-tcp-tls |
| R9 | Browser defences are on by default: CSRF with masked tokens rotated at login, COOP/CORP/Permissions-Policy. | L1, L5, L2 (headers) | `l1_*`, `l5_*`, `l2_recommended_headers_include_coop_corp_permissions_policy` | r11-sec-csrf-headers, r11-sec-auth-state |
| R10 | Every weakening is a named `dangerous_*`/`insecure_*` opt-out that is recorded, and the prod profile refuses to start with an unaccepted one; security state that must be shared (TOTP replay, login throttle) has a persistent store. | M18 and every opt-out | `id_effect::observability::posture` registry; `scripts/check-dangerous.sh` (every `dangerous_*` records); `posture_prod_fails_on_unaccepted_opt_out`; `m18_pg_totp_replay_rejected_across_instances` | r11-sec-adr0013, r11-w3-kit-template, r11-sec-auth-state |

### The posture registry (R10)

`id_effect::observability::posture` is the one place opt-outs are recorded, in the core crate so
every battery can reach it without a new dependency:

- `record_dangerous(id: &'static str, reopens: &'static str)` records that the process took the
  opt-out `id`, which reopens `reopens` (one sentence naming the finding or risk). It records once
  per id, process-wide; the first text wins.
- `dangerous_opt_outs()` returns the recorded `(id, reopens)` pairs sorted by id.

The registry costs nothing until the first call: a process that takes no opt-out allocates
nothing and lists nothing. Tests `posture_is_empty_by_default`,
`posture_records_each_opt_out_once` and `posture_lists_reason_text`
(`crates/id_effect/tests/posture.rs`) pin this behaviour.

### Naming and recording opt-outs

- Every API that weakens a default is named `dangerous_*` (it removes a check: unguarded routes,
  cross-tenant access, raw SQL, followed redirects) or `insecure_*` (it removes a cryptographic or
  transport property: plaintext, unauthenticated peers).
- Every such API calls `record_dangerous` when the opt-out is taken, with its own path as the id.
  `scripts/check-dangerous.sh` fails CI on a `dangerous_*`/`insecure_*` item that does not.
- A behaviour that a semver break removes keeps one minor version as an L9 deprecated shim; the
  shim records itself the same way.
- The prod posture check (`r11-w3-kit-template`) reads `dangerous_opt_outs()` and refuses to start
  the prod profile while any recorded id is not in the operator's accepted list
  (`posture_prod_fails_on_unaccepted_opt_out`). Dev and test profiles report the list and start.

## Consequences

Positive:

- The default path is the safe one, so a new service, a template and a reference app inherit the
  rules without opting in. The Ledger KPI (prod posture clean, zero accepted risks) shows it end
  to end.
- Every weakening is visible: in the code by its name, at run time in the registry, and at prod
  start by the posture check. Reviews search for `dangerous_`/`insecure_` instead of reading for
  absent checks.
- Each rule has a mechanical check, so a regression fails CI rather than waiting for the next
  review.

Negative:

- Defaults change, and some are semver breaks (typestate routes, strict `query`/`execute`,
  bounded mailboxes, no implicit redirects). Each needs an L9 shim and a `docs/MIGRATION-0.11.md`
  entry, and existing users have to declare access or take a named opt-out.
- The secure stack costs something per request (load shedding, redaction, escaping, SSRF
  resolution checks). The bench gate's secure-stack ratio bounds it; it is not zero.
- The registry is process-wide state in the core crate. It is small and append-only, but it is
  global, and tests that record into it share it within one process.
- The posture check is only as good as the recording. `scripts/check-dangerous.sh` catches
  missing calls by name; a weakening that is not named `dangerous_*`/`insecure_*` escapes it, so
  review still has to enforce the naming.

## Alternatives considered

- **Fix each finding in place, without rules.** Rejected: the findings share one pattern
  (opt-in safety), and the next battery would repeat it. The KPI asks for enforcement per rule.
- **Safe defaults with silent opt-outs (a boolean flag, a config key).** Rejected: an unnamed,
  unrecorded opt-out cannot be found in review or refused at prod start, which is the gap the
  review found.
- **Remove the unsafe behaviour entirely.** Rejected for 0.11: some uses are legitimate (local
  plaintext meshes in tests, raw SQL in migrations, public read-only APIs), and L9 needs a shim for
  one minor version. A recorded opt-out keeps them possible and visible.
- **A registry per crate, or a `tracing` event per opt-out.** Rejected: the prod posture check
  needs one list it can read at start, and logs are neither queryable at start nor guaranteed to
  be enabled. A core-crate registry gives one list with no new dependency.
