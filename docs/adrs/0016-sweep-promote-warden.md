# ADR 0016 — Sweep, promote and Warden

## Status

Proposed (2026-11-03). Release 0.14.0 "Sweep & promote", track T13 with E13.1 and E13.2, leaf
`r14-t0-adr0016`. The owner moves it to Accepted at the 0.14 exit (release spec exit gate "ADR 0016",
[0.14.0 D4](../../.maestro/specs/releases/0.14.0.md)); until then every leaf that cites a row of the
Security table (§7) cites it as Proposed. The decisions come from the release spec's "Owner
decisions" table: D1, D3, D4 and D6 are defaults the owner may overturn; D2, D5 (2026-10-29) and D11
(2026-11-03) are owner decisions. D7 (publish the 0.10.1 GHSAs, fund `.github/FUNDING.yml`) is an
owner step, D8 (the scoped `id_effect_fp` exception) belongs to ADR 0010 Amendment 10
(`r14-t0-skeletons`), D9 (Kafka builds on Windows) binds `r14-s-kafka`, and D10 (the features that
never shipped) binds `r14-s-core-runtime`, `r14-s-tool-generate` and `r14-s-deploy-k8s`; this ADR
records them only where they shape a rule below. Where this ADR and PLAN-OF-PLANS disagree,
PLAN-OF-PLANS wins.

Owner decisions already taken (not re-asked): everything buildable ships before 1.0 (L15); reference
apps are the consumers of every row at level production (L4); batteries are effect-native and built
on proven Rust crates (L2); no live AI vendor call in CI (0.13 D8, ADR 0015 §8); GraphQL is a
non-goal.

## Context

At 0.13.0 (measured, `docs/scorecard/results/v0.13.0.json`) id_effect scores E 86.8 (#1, .NET 78.8),
RT 88.7 (#1), ≥80 64, with 3 red rows (`eco.backing`, `eco.community`, `eco.production`). Of 81 scored
rows, 60 are at level `production`, 19 at `documented` and 2 `absent`; 84 checks are open
(`jq -r -f .maestro/specs/releases/0.14/open-checks.jq docs/scorecard/scorecard.json`).

Two gaps remain that no single battery closes:

- **Small open checks across many rows.** Most rows below 100 miss one or two checks that are a day
  or two of work each (selective recovery, cycle detection, upcasting, a Kafka broker, multipart
  uploads, inbound mail, a seed command). PLAN-OF-PLANS §5 T13 calls this E13.1 "Sweep".
- **Rows stuck at `documented`.** A row reaches level `production` only when a reference app uses it
  (METHOD §4). Ledger, Relay and Pipeline exercise the web, data, RPC and breadth batteries, but none
  of them runs AI agents, a real broker or durable workflows, and nineteen rows (STM, FSMs,
  supervision, the mesh, the toolchain) have no consumer in CI. PLAN-OF-PLANS calls this E13.2
  "Promote" and names a fourth reference app, the Warden, without defining it.

Every check the sweep meets adds a surface (a broker consumer, an upload route, an inbound webhook,
a profiling endpoint, a fault toggle, a dev error page, a runtime cron table, Kubernetes manifests,
an agent runtime), and ADR 0013 must cover each from its first commit. This ADR fixes the Warden,
the broker crates, the promotion rule, the sweep scope, the stored targets and the rule each new
surface obeys.

## Decision

### 1. The Warden reference app (D1)

**Warden** (`examples/reference/warden`, package `warden`, tier `example`, `publish = false`, built
only through `id_effect_kit`: its manifest names `id_effect_kit` and `id_effect`, so the workflow,
FSM and node crates are reached through kit features) is a supervised, durable, capability-typed
agent runtime. It is the reference app for what Ledger, Relay and Pipeline do not use: AI agents, a
real broker and durable workflows.

1. **Durable agent runs.** Each run is an `id_effect_workflow` workflow with Postgres history. Its
   steps are model calls and tool calls; a run survives a process restart and resumes from history
   without repeating a tool effect.
2. **Deterministic AI.** Model calls go through `id_effect_ai`: `ScriptedModel` in the app and its
   tests, plus the wiremock vendor fixtures for the OpenAI and Anthropic adapters. No CI job calls a
   live vendor and no API key exists in CI (ADR 0015 §8).
3. **Capability-typed tools.** A tool's type declares the capabilities it may use; its handler runs
   in an environment narrowed to them. A hostile prompt cannot call an unlisted tool (ADR 0015 §9),
   a tool cannot reach a capability it did not declare (compile time plus a runtime test), and a
   write tool runs only under `ToolPolicy::allow_writes`.
4. **Lifecycle and budgets.** The run lifecycle (queued → running → waiting → done / failed /
   cancelled) is an `id_effect_fsm` machine; invalid transitions are refused and entry actions
   journal the status. A per-tenant token and tool budget ledger is shared state under STM: a charge
   is atomic, an exceeded budget commits nothing, and a waiting run blocks without spinning.
5. **Work intake.** Tasks arrive on Redis Streams and on NATS JetStream (`id_effect_nats`, D11), and
   through `id_effect_jobs` queues and cron. Inbox deduplication runs before any tool effect, so a
   redelivered message never repeats one; a NATS message is acked only after its inbox record.
6. **Supervision and the mesh.** Three nodes form a mesh (`id_effect_node`: membership, mutual TLS
   with the cookie handshake, remote calls) and place agents by key in app code (the node crate has
   no key-based placement; `dist.placement#6` stays 0.16). A supervisor tree per node restarts
   crashed agents, with significant children and `max_children` limits. When a node is killed, its
   agents are re-placed and resume from history.
7. **Operations.** `/_console` (fiber dump, CPU profile, stack dump) mounted behind the Warden's
   auth guard; runtime fault toggles wired to model, tool and broker calls; a toxiproxy between the
   nodes and between nodes and brokers in the chaos tests; the prod posture clean with **zero
   accepted risks** (no `dangerous_*` opt-out recorded, every route declared, `AcceptedRisks` empty).

The exit KPI (PLAN-OF-PLANS §6 row 0.14.0): CI job `reference-warden` green on the tag, with
Postgres, Redis, NATS JetStream and toxiproxy containers (`ID_EFFECT_REQUIRE_PG=1`,
`ID_EFFECT_REQUIRE_DOCKER=1`, nothing skipped); three nodes; the node-kill, broker-redelivery and
network-fault tests green; the prod posture clean. The app is built by three leaves split by
directory, `r14-w-warden-core` (W3: domain, workflow, AI, tools, FSM, budget, jobs, HTTP),
`r14-w-warden-mesh` (W4: node, placement, supervisor, brokers, coordination, deadlines) and
`r14-w-warden-ops` (W5: console, chaos, posture, README, the CI job), so each agent's context stays
bounded.

### 2. Broker crates (D2, D11)

| Crate | Client | Tier at entry → exit | Kit feature |
|---|---|---|---|
| `id_effect_kafka` (D2) | `rdkafka` with `cmake-build` (exact pin); consumer groups, contiguous offset commit | integration → integration | `kafka`, off by default and **not** in `full` (needs cmake and a C compiler) |
| `id_effect_amqp` (D2) | `lapin` (exact pin); publisher confirms, prefetch | integration → integration | `amqp` |
| `id_effect_nats` (D11) | `async-nats` JetStream (exact pin, default features off): a stream per topic, durable pull consumers as groups, ack / nak / term | integration → **platform** once the Warden consumes it (`r14-w-warden-mesh`) | `nats`, in `full` (pure Rust) |

- Each implements `MessageBroker` (`crates/id_effect_messaging/src/broker.rs:404`) as
  `RedisStreamsBroker` does (`crates/id_effect_redis/src/streams.rs:333`): the same ack, nack,
  dead-letter, max-deliveries, prefetch and outbox-relay semantics, with test names mirroring
  `crates/id_effect_redis/tests/streams.rs`. Each gets a container in `id_effect_test` and runs in
  the `containers-integration` job.
- ADR 0010 Amendment 10 (`r14-t0-skeletons`) admits the three crates;
  `scripts/validate-crate-packaging.sh` publishes them after `id_effect_messaging` and before kit.
- The owner prefers NATS (D11): the Warden takes its broker intake from Redis Streams and NATS, not
  Kafka. Kafka still ships, tested, for users who want it, off the Warden and the critical path, and
  it builds and passes its unit tests on the Windows leg of the `matrix` job with no per-OS exclusion
  (D9). `async-nats` pulls its own TLS stack; the workspace keeps one rustls, one rcgen, one quinn and
  one tungstenite (`cargo tree -d`).
- `msg.brokers#1` (Kafka) and `#2` (AMQP) are met by these crates; `#3` was already met, so NATS moves
  no row score (D11) but moves `msg.brokers` toward production through the Warden.

### 3. The promotion rule (D3)

A row moves from `documented` to `production` (METHOD §4) only when **a reference-app test that CI
runs exercises each of the row's met checks, and the level audit cites that test**. A test in the
crate itself, a book example or an un-run reference test does not count. `r14-x-level-audit` writes,
per promoted row, the check, the test, the file and the CI job, at HEAD; each reference app's README
carries a "Scorecard evidence" table (row, check, test, file) that the audit reads.

The 19 rows promoted in 0.14 and their consumers:

| Reference app (leaf) | Rows promoted |
|---|---|
| Warden (`r14-w-warden-core`) | `conc.stm`, `fault.fsm` |
| Warden (`r14-w-warden-mesh`) | `fault.supervision`, `dist.membership`, `conc.coordination`, `core.race` |
| Ledger (`r14-p-ledger`) | `data.fp`, `test.property`, `test.determinism`, `core.ergonomics`, `conc.parallel` |
| Relay (`r14-p-relay`) | `conc.perf` (load test with p99 in CI) |
| all four (`r14-p-toolchain`) | `dx.scaffold`, `dx.cli`, `dx.docs`, `dx.diagnostics`, `dx.lint`, `dx.compile`, `dx.deploy` |

`core.ergonomics`, `conc.perf` and `dist.membership` change level but not score: their open checks are
0.16/0.17 or external. Promotion adds +11 points on `conc.stm`, `fault.fsm`, `test.property` and
`dx.compile` (89 → 100, the `documented` cap lifted). The rows kept at production through code the
leaves edit (`web.rpc`, `web.realtime`, `dist.tls`, `data.orm`, `web.ui`, `web.admin`, `ai.llm`,
`eco.i18n`, `eco.tenancy`) may not drop (`scorecard.sh --diff 0.13.0 0.14.0`).

### 4. Sweep scope (D5)

The slot names "the 30 small open checks from the 0.9 gap analysis"; that list is not in the
repository, so the kickoff re-derived it from the open checks at v0.13.0 and kept every check not
slotted to 0.15–0.17 or blocked on people and time. The owner confirmed the list on 2026-10-29.
**37 checks**, each met with a path, a test, a book chapter and an example (METHOD §7.2):

| Row(s) | Checks | Leaf |
|---|---|---|
| core | `core.typed-effect#5`, `core.cause#7` | `r14-s-core-errors` |
| core, conc | `core.interrupt#6`, `core.race#8`, `conc.fibers#2`, `conc.coordination#8` | `r14-s-core-runtime` |
| core | `core.di#8` | `r14-s-core-di` |
| conc, fault | `conc.parallel#8`, `fault.supervision#8` | `r14-s-parallel-supervision` |
| data | `data.fp#7`, `data.fp#8` | `r14-s-data-fp` |
| data | `data.events#7` | `r14-s-events-upcast` |
| msg, eco | `msg.brokers#1`, `eco.fit#6` | `r14-s-kafka` |
| msg | `msg.brokers#2` | `r14-s-amqp` |
| msg | `msg.cron#6` | `r14-s-cron-persistent` |
| msg | `msg.email#6`, `msg.email#7` | `r14-s-mail` |
| web | `web.storage#6` | `r14-s-web-uploads` |
| sec | `sec.authz#7` | `r14-s-authz-views` |
| obs | `obs.console#7` | `r14-s-console-profiling` |
| test | `test.unit#4`, `test.determinism#8` | `r14-s-testing` |
| test | `test.chaos#5`, `test.chaos#6` | `r14-s-chaos` |
| dx | `dx.scaffold#2`, `dx.repl#6` | `r14-s-tool-generate` |
| dx | `dx.cli#4`, `dx.cli#5` | `r14-s-cli-prompts` |
| dx | `dx.deploy#6` | `r14-s-deploy-k8s` |
| dx | `dx.docs#7`, `dx.diagnostics#8`, `dx.lint#6`, `dx.lint#8` | `r14-s-dx-docs-lint` (+ `r14-s-dev-error-page`) |
| eco, sec | `eco.stability#4`, `eco.backing#8`, `sec.supply#7` | `r14-s-governance` |

The same list is the `sweep` definition of `.maestro/specs/releases/0.14/plan-targets.jq`, which
computes the stored targets (§6). Out of scope (release spec non-goals): the E13.3 medium items
(0.15), the AppHost (0.15), the scale and ergonomics items (0.16, 0.17), the checks Rust cannot meet
(`conc.safety#8`, `obs.tracing#8`) and the people and time checks.

### 5. Follow-ups carried from 0.13

| # | Follow-up | Closed by |
|---|---|---|
| F53 | Postgres and Redis idempotency stores for RPC | `r14-f-idempotency-pg` (W2) |
| F59 | Relay exercises server and client streams, SSE, postcard, batching and WebSocket resume | `r14-p-relay` (W4) |
| F62 | h3spec findings in `crates/id_effect_axum/src/server/h3.rs`; `http3-conformance` joins `ci-pass` | `r14-f-h3spec` (W3) |
| F66 | sqlx-sqlite per-row cost; the eager pair limit (0.45 since 2026-10-24) back toward 0.25, at most 0.30 | `r14-f-sqlite-per-row` (W0; it stops and escalates rather than lowering less) |
| `sec.supply#7` | a published GHSA history for the 0.10.1 advisories | `r14-s-governance` verifies SECURITY.md against the published advisories; publishing them is the owner's (D7). If they are still unpublished at the tag, the row is carried in writing |

F63, F64 and F65 were fixed in 0.13 (0b6d41cc, 00e4edc9).

### 6. Stored targets (D6)

Unlike 0.13 (D13, no edit), 0.14 stores its row targets: there were no stored 0.14.0 targets to keep.
The 35 rows of the release spec's `scorecard.rows_in_scope` are written as `targets["0.14.0"]` in
`docs/scorecard/scorecard.json` by `jq --tab -f .maestro/specs/releases/0.14/plan-targets.jq` (every
sweep check met, every promoted row at level production, score = round(100 × met / scored checks)
capped by the level), then formatted with `biome format`. Only `targets` objects change; no measured
score, level, met list or evidence moves.

| Row | 0.13.0 → 0.14.0 |
|---|---|
| `core.typed-effect`, `core.di`, `core.cause`, `core.interrupt`, `core.race` | 88 → 100 |
| `conc.fibers`, `conc.coordination`, `conc.parallel`, `fault.supervision`, `data.events`, `sec.authz`, `sec.supply`, `msg.cron`, `test.unit`, `test.determinism`, `dx.scaffold`, `dx.docs`, `dx.deploy` | 88 → 100 |
| `conc.stm`, `fault.fsm`, `test.property`, `dx.compile` | 89 → 100 (promotion) |
| `data.fp`, `msg.brokers`, `msg.email`, `test.chaos`, `dx.cli`, `dx.lint` | 75 → 100 |
| `web.storage`, `obs.console`, `dx.diagnostics`, `dx.repl`, `eco.stability` | 75 → 88 |
| `eco.fit` | 63 → 75 |
| `eco.backing` | 38 → 50 |

`bash scripts/scorecard.sh --targets 0.14.0` on the measured 0.13.0 baseline then reads **E 92.8
(#1), RT 94.8 (#1), ≥80 75, red 2** (`eco.community`, `eco.production`; `eco.backing` leaves red at
50), leader 64, so the 0.14 index gate is **91.8** (planned − 1.0). Before storing, the tool carried
the 0.13.0 measurement forward: E 87.1, RT 88.8, red 3. `--targets 0.15.0` … `--targets 0.17.0` now
carry the 0.14.0 plan (PLAN-OF-PLANS §6 †), and `--targets 1.0.0` reads E 93.7, RT 95.2, red 1; METHOD
§5.2 and the self-test copy in `scripts/scorecard.sh` are restated to it, and the 1.0.0 cell of
PLAN-OF-PLANS §6 is left for the retro. Stored results (`--json 0.6.0` … `0.13.0`) are unchanged.

The plan needs every one of the 37 checks: missing about eight rows breaks the −1.0 gate. Each missed
row is carried in writing at exit; the release spec's scope-cut table orders what may be cut, and
the Warden, NATS and promotion leaves are never cut.

### 7. Security rules for the 0.14 surfaces (ADR 0013 applied)

Every new surface follows ADR 0013 from its first commit: deny by default, finite limits before
buffering, encrypted transports, every weakening a named `dangerous_*` opt-out that calls
`id_effect::observability::posture::record_dangerous` and is listed by `scripts/check-dangerous.sh`,
and a prod posture that refuses to start on an unaccepted one. Rule numbers are **ADR 0013's**: R1
deny by default, R2 typed secrets, R3 redacted logs, R4 bounded untrusted input, R5 raw SQL needs a
capability, R6 guarded outbound requests, R7 operator surfaces closed in prod, R8 encrypted and
authenticated transports, R9 browser defences, R10 named `dangerous_*` opt-outs and the prod posture.
The release spec's table "Security — ADR 0013 applied to the new surfaces" uses the same numbers and
the same compile-fail ids.

Each rule holds while its enforcement point is green. The test ids are the ones the owning leaf's
acceptance criteria name (binding there); the new security-only cases of `r14-e-fuzz-security` carry
the rule they enforce as a prefix (`r4_…`, `r7_…`). `tests/security_compile_fail/tests/surface_014.rs`
(`r14-e-fuzz-security`) asserts that every test id of the release spec's Security table exists and
that every fuzz target named there is a `[[bin]]` with seeds.

| # | Surface | Rule (default) | ADR 0013 | Enforcement point (mechanical) | Leaf |
|---|---|---|---|---|---|
| S1 | Kafka consumers and producers | TLS on by default (`security.protocol` SSL); plaintext only through `KafkaConfig::dangerous_plaintext()`, recorded; SASL passwords are `Secret`; record ≤ 1 MiB and ≤ 64 headers / 16 KiB, checked before decode; an oversize record is dead-lettered without its payload. | R8, R10, R2, R4 | `kafka_plaintext_requires_dangerous_opt_out`, `kafka_oversize_record_is_dead_lettered`; fuzz `kafka_message_headers` (`decode_record`); `scripts/check-dangerous.sh` | r14-s-kafka, r14-e-fuzz-security |
| S2 | AMQP consumers and producers | `amqps://` by default; `amqp://` only through `AmqpConfig::dangerous_plaintext()`, recorded; the URL password is a `Secret`, redacted in errors; body ≤ 1 MiB (also the channel `frame_max`) and ≤ 64 headers / 16 KiB before decode. | R8, R10, R2, R4 | `amqp_plaintext_requires_dangerous_opt_out`, `amqp_oversize_message_is_dead_lettered`; fuzz `amqp_properties` (`decode_properties`); `scripts/check-dangerous.sh` | r14-s-amqp, r14-e-fuzz-security |
| S3 | NATS JetStream consumers and producers | TLS required by default; plaintext only through `NatsConfig::dangerous_plaintext()`, recorded; token, password, NKey seed and creds are `Secret`; payload ≤ 1 MiB and ≤ 64 headers / 16 KiB before decode. | R8, R10, R2, R4 | `nats_tls_round_trip`, `nats_plaintext_requires_dangerous_opt_out`, `nats_oversize_message_is_dead_lettered`, `decode_message_rejects_oversize_headers`; fuzz `nats_message_headers` (`decode_message`); `scripts/check-dangerous.sh` | r14-s-nats, r14-e-fuzz-security |
| S4 | Multipart uploads | An upload route cannot be mounted without a per-file byte cap; whole-body cap, ≤ 8 parts, ≤ 64 KiB per text field; content types from an allow-list, magic bytes must agree; the storage key is server-generated, never from the client filename; the CSRF check stays on. | R4, R1, R9 | compile-fail `r4_upload_route_without_size_cap`; `upload_over_file_cap_is_413`, `upload_part_and_field_counts_are_capped`, `upload_with_type_outside_allow_list_is_415`, `upload_whose_magic_bytes_contradict_its_type_is_415`, `upload_filename_hint_is_sanitised`, `store_upload_key_never_derives_from_client_filename`, `upload_route_keeps_csrf_check`; fuzz `web_multipart_upload` (`upload::read_parts`) | r14-s-web-uploads, r14-e-fuzz-security |
| S5 | Inbound mail | Webhook body ≤ 10 MiB; MIME depth ≤ 8, ≤ 100 parts, ≤ 1 000 header lines, checked before the tree is built; the route's access decision is the signature check with a timestamp window, no unsigned mode except `dangerous_unsigned_inbound()` (recorded, local tests); unrouted mail is refused; bodies and recipients are not logged. | R4, R1, R10, R3 | `inbound_body_over_cap_is_413`, `inbound_mime_depth_and_part_caps_hold`, `inbound_webhook_rejects_missing_or_bad_signature`, `inbound_webhook_rejects_a_stale_timestamp`, `unrouted_inbound_mail_is_rejected`; fuzz `mail_inbound_mime` (`parse_mime`); `scripts/check-dangerous.sh` | r14-s-mail, r14-e-fuzz-security |
| S6 | Multi-channel notification senders | Webhook targets pass the platform `SsrfPolicy` at construction and the client guard per request; webhook URLs and keys are `Secret`; the database channel scopes rows to their notifiable. | R6, R2, R1 | `webhook_channel_refuses_a_private_network_target`, `database_channel_scopes_rows_to_the_notifiable` | r14-s-mail |
| S7 | Profiling endpoint (CPU profile, stack dump) | Off unless enabled in code; reachable only inside the guarded console router (no public constructor); window ≤ 30 s, a non-zero rate, one profile at a time, dump size capped; the enabled state is reported to the posture registry. | R7, R4 | compile-fail `r7_profiling_endpoint_without_guard`; `cpu_profile_is_guarded`, `every_console_route_is_guarded_over_a_socket`, `cpu_profile_is_absent_unless_enabled`, `cpu_profile_window_and_rate_are_capped`, `stack_dump_is_capped`; posture probe `console_profiling_reported_in_posture` | r14-s-console-profiling, r14-e-fuzz-security |
| S8 | Runtime fault toggles | Inert under profile `prod`; a `[faults]` table that enables a point in prod records `FaultToggles::dangerous_enabled_in_prod`, and the prod posture refuses to start unless the operator accepted it; no HTTP surface. | R10 | `prod_profile_keeps_toggles_inert`, `prod_profile_enabled_toggle_records_dangerous`, `prod_posture_refuses_enabled_fault_toggles`; `scripts/check-dangerous.sh` | r14-s-chaos |
| S9 | Dev error page | Only under profile `dev`; in `test` and `prod` the response is byte for byte the opaque `500`, and no API enables it elsewhere; source read only under the canonical source root (no symlink or `..` escape), ≤ 1 MiB per file, ≤ 16 frames; every value escaped. | R7, R9 | `prod_profile_response_is_the_opaque_500`, `test_profile_response_is_the_opaque_500`, `source_outside_root_is_never_read`, `symlink_escape_is_never_read`, `defect_message_html_is_escaped` | r14-s-dev-error-page |
| S10 | Event upcasters | A stored payload over `max_payload_bytes` (default 1 MiB) fails before it is parsed; steps are pure JSON functions; a failing step names stream, version and step, never the payload. | R4, R3 | `upcast_rejects_a_payload_over_the_cap`, `a_failing_step_names_stream_version_and_step`; fuzz `event_upcast` (`Upcasters::upcast`) | r14-s-events-upcast, r14-e-fuzz-security |
| S11 | Runtime-mutable cron schedules | A schedule may name only a registered job (deny by default); ≤ 1 000 schedules and ≤ 64 KiB payload per row, checked on `upsert`; cron expression and zone validated; the crate's own parameterised SQL only. | R1, R4, R5 | `schedule_for_unregistered_job_is_rejected`, `schedule_count_is_capped`, `upsert_rejects_invalid_cron_or_zone` | r14-s-cron-persistent |
| S12 | Kubernetes manifests (`id-effect deploy k8s` / `helm`) | Pod Security "restricted" out of the box (non-root, read-only root filesystem, no privilege escalation, all capabilities dropped, `RuntimeDefault` seccomp, no service-account token, resource limits); secrets only through `secretKeyRef`; the image needs a tag or digest; no flag weakens these. | R1, R2 | `manifests_meet_pod_security_restricted`, `deploy_refuses_latest_or_untagged_image`; `scripts/k8s-validate.sh` (kubeconform `-strict`) | r14-s-deploy-k8s |
| S13 | Warden agent tools | Registered tools only, an unknown name is refused and never runs; each tool sees only its declared capabilities; write tools need `allow_writes`. | R1 | `warden_prompt_injection_cannot_call_unlisted_tool`, `warden_tool_sees_only_declared_capabilities`, `warden_write_tool_denied_without_allow_writes` | r14-w-warden-core |
| S14 | Warden budgets | The tool loop is bounded (ADR 0015 §9 defaults); per-tenant token and tool budgets are charged atomically under STM, and an exceeded budget commits nothing. | R4 | `warden_tool_loop_bounded`, `warden_budget_charge_is_atomic`, `warden_budget_exceeded_commits_nothing` | r14-w-warden-core |
| S15 | Warden run history and intake | The run history stores model replies, never keys, and prompts are not logged (`dangerous_log_prompts` is not taken); a resumed run or a redelivered message never repeats a tool effect (inbox before effect, NATS ack after the inbox record); every subscription has prefetch and size caps. | R3, R2, R4 | `warden_resumed_run_does_not_repeat_tool_effects`, `warden_broker_redelivery_no_duplicate_tool_effects`, `warden_nats_ack_after_inbox_record`; `warden_prod_posture_is_clean` (no `dangerous_*` recorded) | r14-w-warden-core, r14-w-warden-mesh, r14-w-warden-ops |
| S16 | Warden mesh | Mutual TLS (`mesh-mtls`) with the cookie handshake; no plaintext transport; broker connections over TLS with no opt-out taken. | R8 | `warden_node_without_cookie_or_cert_rejected`, `nats_tls_round_trip`; `warden_prod_posture_is_clean` | r14-w-warden-mesh, r14-w-warden-ops |
| S17 | Warden operations and prod posture | `/_console` and profiling behind the Warden's auth guard and closed in prod unless guarded; every route declared (`.public()` for `/health` only); the prod profile refuses to start with a fault toggle enabled, a failed probe or any `dangerous_*` recorded; zero accepted risks. | R7, R10, R1 | `warden_console_requires_auth`, `warden_console_profiling_closed_in_prod`, `warden_prod_posture_is_clean`, `warden_prod_refuses_to_start_with_fault_toggles_enabled`, `warden_prod_refuses_to_start_on_a_failed_probe` | r14-w-warden-ops |

**The ADR 0013 rules a 0.14 surface touches**, grouped:

| ADR 0013 rule | 0.14 surfaces | Enforcement at 0.14 |
|---|---|---|
| R1 deny by default | upload routes, inbound webhook, notification rows, cron job names, pod privileges, Warden tools and routes | S4, S5, S6, S11, S12, S13, S17 |
| R2 secrets typed and redacted | broker credentials, mail keys and webhook URLs, manifest secrets, vendor keys | S1–S3, S6, S12, S15 |
| R3 logs redacted by default | mail bodies, upcast errors, prompts and run history | S5, S10, S15 |
| R4 untrusted input bounded | broker messages and headers, multipart bodies, MIME, profiles, historic payloads, cron rows, agent loops and budgets | S1–S5, S7, S10, S11, S14, S15; six new fuzz targets; compile-fail `r4_upload_route_without_size_cap` |
| R5 raw SQL needs a capability | cron schedule store | S11 (no caller SQL; `m16_query_with_string` and `raw_sql_non_literal` keep holding) |
| R6 outbound requests guarded | notification webhooks | S6 |
| R7 operator surfaces closed in prod | profiling endpoint, dev error page, Warden console | S7, S9, S17; compile-fail `r7_profiling_endpoint_without_guard` |
| R8 transports encrypted and authenticated | Kafka, AMQP, NATS, the Warden mesh | S1–S3, S16 |
| R9 browser defences on | upload CSRF, dev error page escaping | S4, S9 |
| R10 every weakening recorded | broker plaintext, unsigned inbound mail, fault toggles in prod, the Warden's posture | S1–S3, S5, S8, S17; `scripts/check-dangerous.sh`; `posture_prod_fails_on_unaccepted_opt_out` (0.11) |

The security KPIs for 0.14 (release spec `security_kpis`): open highs 0; ≥ 6 new cargo-fuzz targets
(≥ 29 in total) in ClusterFuzzLite with no open crash; compile-fail security cases +2; the
secure-stack (≤ 5 µs) and unary-RPC (≤ 3 µs) budgets unchanged. THREAT-MODEL.md gains a "Sweep
surfaces (0.14)" section (`r14-e-fuzz-security`).

## Consequences

Positive:

- One reference app exercises AI agents, a real broker, durable workflows, FSMs, STM, supervision
  and the mesh together, so the exit re-score can cite one CI job (`reference-warden`) for production
  level on the rows no other app reaches.
- The promotion rule ties every `production` level to a CI-run test, which the level audit lists;
  a level can no longer rest on a crate test or on prose.
- The stored targets make the plan reproducible by the tool (`--targets 0.14.0`,
  `--compare-targets 0.14.0`) instead of by a jq copy.
- Every new surface inherits ADR 0013 from its first commit: brokers are TLS by default, uploads
  cannot be mounted without a cap, profiling cannot be mounted without a guard, fault toggles and the
  dev page are inert in prod.

Negative:

- Three new crates (`id_effect_kafka`, `id_effect_amqp`, `id_effect_nats`) for the 0.18 crate review to
  justify. rdkafka needs a C toolchain and cmake in devenv and CI, including Windows (D9), and adds
  build time (K6 delta reported); it stays out of kit `full`.
- `reference-warden` is a fourth container job (Postgres, Redis, NATS, toxiproxy) on an already long
  CI; the Warden is the critical path across three waves.
- The planned index needs all 37 checks; a miss of about eight rows breaks the gate and must be
  carried in writing.
- AI in the Warden is scripted. Agent behaviour against live vendors is not proven, only the
  library's bounds on what a model can make the program do.
- `sec.supply#7` depends on an owner step for the third release running.

## Alternatives considered

- **Promote rows through the existing three apps only (D1).** Rejected: none of them runs agents, a
  real broker or durable workflows, so `conc.stm`, `fault.fsm`, `fault.supervision`,
  `dist.membership` and the broker rows would stay `documented`.
- **Kafka as the Warden's broker (D2 before D11).** Rejected by the owner (D11): NATS is pure Rust,
  lighter in CI and preferred; Kafka still ships for users.
- **One broker crate with features (D2).** Rejected: rdkafka's C build would leak into every user of
  the crate; separate crates keep the cost where it is chosen.
- **Count crate tests as production evidence (D3).** Rejected: METHOD §4 defines `production` as use
  by a reference app; a crate test proves the feature, not its use.
- **Store a smaller committed set of targets (D6, Open question 3).** Rejected by default: every
  check of the sweep has a leaf, and the scope-cut table already orders what may be dropped.

## Open questions (defaults taken)

1. **D6:** store all 35 rows at 0.14.0 (E 92.8), or a smaller committed set with the rest as margin?
   Default: all 35.
2. **D1 / D11:** the Warden on Redis Streams and NATS in CI (`reference-warden` runs four containers)?
   Default: yes.
3. **Security table numbering:** rows S1…S17 (0.13 used B1…B23); new security-only test ids carry
   the ADR 0013 rule as a prefix, the leaves' other test names stay as their specs name them.
   Default: as written.
4. **D3:** does a promoted row whose open check is 0.16/0.17 or external (`core.ergonomics`,
   `conc.perf`, `dist.membership`) move level without score? Default: yes.

## References

- Release spec: `.maestro/specs/releases/0.14.0.md` (D1–D11, Security, leaves); leaf
  `.maestro/specs/releases/0.14/r14-t0-adr0016.md`; targets `.maestro/specs/releases/0.14/plan-targets.jq`.
- PLAN-OF-PLANS §5 T13 (E13.1, E13.2), §6 row 0.14.0, §11 (2026-11-03).
- ADR 0010 (crate tiers; Amendment 10 by `r14-t0-skeletons`), ADR 0013 (secure by default), ADR 0015
  (breadth batteries: §8 deterministic AI, §9 tool safety).
- `docs/scorecard/METHOD.md` §4 (levels), §5.1 (planned scores), §6 (blind re-score);
  `docs/scorecard/results/v0.13.0.json`, `docs/scorecard/REPORT-0.13.0.md`.
