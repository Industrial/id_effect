# 0.10 level audit (`r10-w3-level-audit`)

Read-only audit at HEAD 81e0c645 (release/0.10.0, 2026-10-17) of the 14 in-scope rows plus
sec.authn against the v2 checks in `docs/scorecard/scorecard.json` and METHOD §7.2. Expected
scores from `scripts/scorecard.sh --file <copy> --version 0.10.0` (audited rows at their audited
0.10.0 level/met; the other 67 rows copy 0.9.0).

## Result

Expected **E 74.2 (#2), RT 77.2 (#1), U 73.6, red 12 (limit ≤ 12), ≥80 45, leader 20**.
Gate (D1/D6): planned 73.8 − 1.0 = 72.8 → passes by 1.4. Strict-assessor floor (obs.tracing#6,
web.client#6, fault.reload#5, dx.repl#3 rejected): E 73.6 (#2), still above Spring (73.0).
With the Ledger using the 0.10 batteries: E 74.4 (74.6 with msg.jobs#7). `--diff 0.9.0 0.10.0`:
14 up, 0 down.

| Row | 0.9.0 | Target | Expected | Met/8 | Level | Gaps |
|---|---|---|---|---|---|---|
| msg.jobs | 13 | 88 | 88 | 7 (no #7) | documented (production pending Ledger) | J1, H1 |
| msg.email | 1 | 75 | 75 | 6 | documented (pending Ledger) | H1, H5 |
| obs.console | 25 | 75 | 75 | 6 | documented (pending Ledger) | — |
| msg.brokers | 25 | 75 | 75 | 6 | documented (pending Ledger) | — |
| dx.repl | 25 | 63 | 63 | 5 | production (0.9) | R3 |
| obs.tracing | 63 | 88 | 88 | 7 | documented | R1 |
| test.chaos | 38 | 75 | 75 | 6 | documented (pending Ledger) | — |
| data.events | 39 | 75 | 88 | 7 | documented | H1 |
| web.storage | 38 | 75 | 75 | 6 | documented (pending Ledger) | — |
| dx.lint | 38 | (0.11: 63) | 38 | 3 | labs | stretch E1.9 |
| fault.reload | 25 | 50 | 63 | 5 | production (0.9) | R2 |
| obs.logging | 75 | 88 | 89 (100 w/ Ledger) | 8 | documented | Ledger |
| dx.scaffold | 75 | 88 | 88 | 7 (no #2) | documented | — |
| web.client | 75 | 88 | 89 (100 w/ Ledger) | 8 | documented | R1, Ledger |
| sec.authn | 75 | 88 | 88 | 7 (no #7) | production (0.9) | H1 |

## Gaps

| Id | Gap | Owner / status |
|---|---|---|
| H1 | CI pg job (`ci.yml:203`) lacks `-p id_effect_events -p id_effect_mail` and features `id_effect_jobs/cron`, `id_effect_events/pg`, `id_effect_auth/accounts`, `id_effect_mail/jobs`, so `cron_pg.rs`, `pg_events.rs`, `live_postgres_account_tokens`, `deliver_later_sends_after_commit_only` never run on Postgres in CI (the containers job has no DATABASE_URL). | main session, after r10-w3-ledger (ci.yml) |
| J1 | msg.jobs#7: console jobs panel is read-only; add guarded `POST /_console/api/jobs/{id}/retry` (+ cancel) via `JobQueue::retry_now`, a dashboard button, a test. Commit 50851c2b's "#7" claim is premature until then. | gap-fix agent |
| L1 | The Ledger uses no 0.10 battery yet (production level, check P, platform tier flips). | r10-w3-ledger (running) |
| R2 | fault.reload#5 (ProfileWatch) has only rustdoc + unit tests: add a book section, an example and an integration test. | gap-fix agent |
| R1 | obs.tracing#6 / web.client#6 "created automatically": TracedHttpClient / InstrumentedSqlClient are opt-in decorators nothing installs; wire them from the kit's OTel feature / the `--otel` template. | main session, after r10-w3-ledger (kit, template) |
| R3 | dx.repl#3 is restart-on-change: document it as live reload in ch34-02 citing `tests/dev.rs:160`. | gap-fix agent |
| H5 | `crates/id_effect_mail/tests/smtp.rs` names a test SMTP server `Stub` (check S grep); rename. | gap-fix agent |
| H6 | Planned evidence paths that shipped elsewhere: ch31-02-brokers.md → ch31-02-messaging.md; ch34-03-generators.md → ch34-00 §Generators; 020_faults → 030_faults; console 010 → 020_console; storage 010 → 020_storage. | exit re-score |
| G1 | 0.11: dx.repl#5 dev dashboard, dx.scaffold#2 resource generator, web.storage#6 multipart files, msg.brokers#1/#2 Kafka/AMQP. | deferred |

Weak / arguable evidence (counted as noted): msg.jobs#7 not met; obs.tracing#6 / web.client#6
met but opt-in (R1); fault.reload#5 met on rustdoc only (R2); fault.reload#4 is deploy config +
rendered-config test; dx.repl#3 restart not hot reload; test.chaos#2 latency in test doubles
(matches the check text); msg.email#1 SendGrid, no SES ("etc."); sec.authn#8 not Ledger-used yet.

Full per-row evidence (file:line, test names) is in the audit agent's report of 2026-10-17; the
scorecard copies used are in the session scratchpad (`audit-{strict,ledger,lenient,pess}.json`).
