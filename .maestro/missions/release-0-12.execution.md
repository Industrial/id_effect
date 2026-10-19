# release-0-12 — execution overlay

Mission: created at kickoff (`maestro mission from-spec .maestro/specs/releases/0.12.0.md`) · spec
`.maestro/specs/releases/0.12.0.md` (approved under the owner's L15 goal; defaults D1–D14 open for
review, owner steps D15/D16) · branch `release/0.12.0` (off the `v0.11.0` tag after `release/0.11.0`
merged). Task ids are filled in by the coordinator when the tasks are created; until then the leaf slug
is the key.

## Preconditions (day 0)

- `v0.11.0` tagged; `docs/scorecard/results/v0.11.0.json` and `REPORT-0.11.0.md` committed (baseline).
- Owner has seen the six open questions in the release spec (defaults apply if unanswered).
- Bench runner allows `sudo tc` (D16) before r12-e12-5-benches and the h3-server spike.

## Wave 0 — head (dispatched at kickoff)

| Agent | Leaf (task id) | Files (owned) |
|---|---|---|
| A0 roadmap | `r12-t0-adr0014` (—) | docs/adrs/0014-one-call-surface.md, docs/scorecard/scorecard.json (targets), PLAN-OF-PLANS.md §5 T12 / §6 / §11, the release spec `scorecard:` block |
| A1 governance | `r12-t0-skeletons` (—) | docs/adrs/0010-crate-tiers.md (Amendment 8), root Cargo.toml members, labs/Cargo.toml, `git mv labs/id_effect_rpc crates/id_effect_rpc`, crates/id_effect_rpc_macros/ skeleton, examples/reference/relay/ skeleton, scripts/validate-crate-packaging.sh, .github/workflows/ci.yml (-p lines, labs job), docs/MIGRATION-0.12.md (headings) |

## Wave 1 (after A1 merges)

| Agent | Leaf (task id) | Files (owned) | Depends on / note |
|---|---|---|---|
| B1 rpc | `r12-e12-1-promote` (—) | crates/id_effect_rpc/** (all), r12-rpc-design.md, book ch07-12, ch29-00, ch33-00 | Skeletons. **Design note reviewed before wave 2's kinds starts.** Declares every rpc feature once. |
| B2 axum | `r12-e12-2-axum-ws-sse` (—) | crates/id_effect_axum/{Cargo.toml, src/realtime/**}, ws/sse tests + examples 090/091, book ch33-05 | Skeletons. Declares axum features ws/sse/tls/http3/webtransport once. |
| B3 quic | `r12-e12-3-quic-endpoint` (—) | crates/id_effect_platform/{Cargo.toml, src/tls/{mod,certs}.rs, src/quic/**}, crates/id_effect_node/{Cargo.toml, src/transport/{certs,quic,mod}.rs}, node tests transport_tls/shared_endpoint, book ch38-01 section | Skeletons (Amendment 8). Highest-risk leaf: review the D7 rule in its PR before merge. |

## Wave 2

| Leaf (task id) | Depends on |
|---|---|
| `r12-e12-1-kinds` (—) | promote + design-note review; critical path, start first |
| `r12-e12-3-tls-h2` (—) | axum-ws-sse (axum Cargo.toml), quic-endpoint (platform `tls`) |
| `r12-e12-3-h3-client` (—) | quic-endpoint |
| `r12-e12-3-certs-acme` (—) | quic-endpoint; Pebble container on its own port |

## Wave 3 (six leaves; at most five at once — start the first two immediately)

| Leaf (task id) | Depends on |
|---|---|
| `r12-e12-2-rpc-ws-sse` (—) | kinds, axum-ws-sse — **critical path** |
| `r12-e12-3-h3-server` (—) | tls-h2 — spike (D9) first; raise Open question 6 at once if HTTP/3 loses |
| `r12-e12-1-batching` (—) | kinds (also edits axum middleware/rate_limit.rs) |
| `r12-e12-1-macro` (—) | kinds, skeletons (macro crate) |
| `r12-e12-1-postcard` (—) | kinds |
| `r12-e12-2-subscriptions` (—) | kinds; own Postgres container (not the devenv DATABASE_URL) |

## Wave 4 (six leaves; at most five at once)

| Leaf (task id) | Depends on |
|---|---|
| `r12-e12-2-bidi-resume` (—) | rpc-ws-sse — **critical path** |
| `r12-e12-2-channels` (—) | subscriptions |
| `r12-e12-2-mesh-rpc` (—) | postcard |
| `r12-e12-4-openapi-asyncapi` (—) | macro |
| `r12-e12-5-benches` (—) | batching, rpc-ws-sse, h3-server, h3-client; idle machine; writes the unary delta gate only after measuring (D5) |
| `r12-e12-3-deploy-guide` (—) | h3-server, certs-acme (docs only; start last) |

## Wave 5

| Leaf (task id) | Depends on |
|---|---|
| `r12-e12-4-gen-ts` (—) | macro, batching, bidi-resume — **critical path** |
| `r12-e12-3-webtransport` (—) | h3-server, bidi-resume (first in the scope-cut order) |
| `r12-e12-5-fuzz-security` (—) | batching, postcard, macro, bidi-resume; TMPDIR under /data |

## Wave 6

| Leaf (task id) | Depends on |
|---|---|
| `r12-e12-4-rpc-check` (—) | gen-ts (cli.rs, rpc/mod.rs) |
| `r12-w6-kit-template` (—) | webtransport, gen-ts, channels, mesh-rpc, certs-acme, h3-server — **critical path** (also takes id_effect_tool Cargo.toml after gen-ts) |

## Wave 7

| Leaf (task id) | Depends on |
|---|---|
| `r12-w7-relay` (—) | kit-template, rpc-check — §6 KPI, never cut; owns the reference-relay job in ci.yml |
| `r12-w7-ledger` (—) | kit-template, rpc-check; owns reference-ledger job steps (rebase after relay) |

## Wave 8 and exit

| Leaf (task id) | Depends on |
|---|---|
| `r12-w8-level-audit` (—) | relay, ledger (read-only) |
| `r12-w8-kpis` (—) | relay, ledger, benches, fuzz-security; idle machine |
| `r12-t0-migration-guide` (—) | every leaf; then the blind re-score, REPORT-0.12.0, CHANGELOG, owner steps D15 (ADR 0014 accepted) |

Critical path: skeletons → promote → kinds → rpc-ws-sse → bidi-resume → gen-ts → kit-template → relay →
level audit. HTTP/3 (quic-endpoint → tls-h2 → h3-server) runs beside it and must be done before
webtransport (wave 5).

## Rules (as 0.10 / 0.11, plus the 0.11 lessons)

- One shared checkout: every leaf's Owns are disjoint within a wave (release spec "File ownership").
  Append-only shared files: CHANGELOG.md, docs/MIGRATION-0.12.md, book SUMMARY.md, Cargo.lock
  (regenerated on rebase), the `lib.rs` of id_effect_rpc / id_effect_axum / id_effect_platform after
  their wave-1 owner, and the module-declaration lines of the rpc `transport/`, `client/`, `server/`
  and axum `server/` `mod.rs` files.
- Agents never commit or stage. The main session reviews each leaf and commits it through a private
  index (`GIT_INDEX_FILE=.tmp/idx-<leaf> git add -- <paths>`, then `git commit -s -F <msg> -- <paths>`):
  pathspec-limited, DCO `Signed-off-by` required. Never run a bare `git commit` (0.10: a stray command
  staged other agents' work).
- Gates set `TMPDIR` under `/data` (the 2 GB `/tmp` tmpfs fills; fuzz and h3 tests are heavy). Live
  tests use `ID_EFFECT_REQUIRE_PG=1` / `ID_EFFECT_REQUIRE_DOCKER=1` / `ID_EFFECT_REQUIRE_BROWSER=1` and
  fail rather than skip; they never read the ambient `DATABASE_URL` (devenv overrides it) — each starts
  its own container on a distinct port.
- Bench gates are absolute where a ratio is ill-posed (the unary RPC delta, the secure-stack delta);
  ratios are kept only between two measurements of the same stack (batch vs sequential, h3 vs h2).
- Exact pins for h3 / h3-quinn / h3-webtransport; `cargo tree -d` must show one quinn, one rustls, one
  tungstenite; `reqwest_unstable` never appears.
- Never send the owner's email or other personal data to external services (crates.io lookups use a
  generic User-Agent).

## Follow-ups recorded during the release

(empty at kickoff)
