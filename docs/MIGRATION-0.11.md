# Migrating to id_effect 0.11

This guide is the "if you used X, now do Y" companion to the `0.11.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.10. If you are on 0.9, apply
[MIGRATION-0.10.md](MIGRATION-0.10.md) first.

Every intentional public-API break in 0.11 is listed here. A break reported by
`cargo semver-checks` (the `semver` CI job, baseline the last 0.10.x release) that is not listed
below is a bug. The deprecation rule and semver policy are in [STABILITY.md](STABILITY.md).

Each 0.11 change fills the matching section below.

## Routing: every route is public or guarded (H8)

Filled by `r11-sec-h8-routing`.

## SQL: `Sql` and `RawSql`

Raw SQL needs a capability (ADR 0013 R5; finding M16, decision D9).

- **`query`, `execute`, `query_as`, `query_one_as`, `query_opt_as`, `query_stream` and
  `query_stream_as` take `impl Into<Sql>` instead of `impl Into<String>`.** Only a
  `&'static str` converts, so a literal or a `const` compiles unchanged (`const BY_ID: Sql =
  Sql::new("…")` works too) and a `String` does not: `query(format!("… {x}"), vec![])` is now
  `the trait From<String> is not implemented for Sql`. Move the value into a bound parameter
  (`query("… WHERE id = $1", params![x])`). The statement helpers of the `query` feature
  (`execute_query`, `fetch_*_as`) and `Repository` are unchanged.
- **SQL that is really built at run time** (a table name from an allowlist, generated DDL) is a
  `RawSql`, run with `query_raw(RawSql::new(sql), params)` / `execute_raw(..)`. Both need a
  `RawSqlCapability` in `R` besides the client; it has no public constructor, and
  `RawSqlCapability::layer_dangerous()` provides it (merge it with the client layer:
  `sql_client_layer(c).merge(RawSqlCapability::layer_dangerous())`, environment
  `HCons<SqlClientService, RawSqlEnv>`). Taking it records the posture opt-out
  `id_effect_sql::RawSqlCapability::layer_dangerous`, which the prod posture check reports.
- The `SqlClient` and `SqlTransaction` trait methods still take `&str` (the driver boundary,
  revisited in 0.18); the `raw_sql_non_literal` lint warns on a non-literal argument there.
- `id_effect_test`: `Fixture::insert`'s default now runs `insert_sql` through
  `SqlClient::query` (it returns a `String`); behaviour is unchanged.

## Redis: `clear()` on an empty namespace

Destructive operations cannot target everything by accident (ADR 0013 R5; finding M17).

- **`RedisKv::clear`, `RedisHash::clear` and `RedisJson::clear` on a store whose namespace is
  `""` fail with the new `RedisError::EmptyNamespace`** instead of deleting every key of the
  database (`RedisError` is `#[non_exhaustive]`; a `match` needs a wildcard already). A namespaced
  store clears only its own `namespace:*` keys, as before. `keys()` / `ids()` are unchanged.
- To flush on purpose, call `clear_all_dangerous()` on the store: it deletes what `clear()` would
  and, for the empty namespace, every key; it records the posture opt-out
  `id_effect_redis::<Store>::clear_all_dangerous`.
- `RedisCacheBackend` on the empty namespace: `invalidate_all` now fails (counted as a backend
  error) instead of flushing the database; give the cache a namespace.

## Authorization: tenant match by default (M19)

Resources scoped to a tenant require the tenant match (ADR 0013 R1; finding M19).

- **Every policy defined for a `Tenanted` resource checks the tenant first.**
  `PoliciesBuilder::define::<Res>`, `check_with` and `authorize_with` put the policy behind the
  new `TenantScoped` check: on a resource whose `TenantScope` is a tenant, a principal of another
  tenant, or of none, is denied with `resource belongs to another tenant` before the policy runs.
  A definition that relied on reaching another tenant's resource (an ACL entry or a role granted
  to a principal outside the tenant) is now denied. Existing `.and(same_tenant())` stays correct
  and is now redundant; `same_tenant()` remains for policies evaluated directly
  (`Policy::evaluate` is the bare rule; wrap it in `tenant_scoped(..)` for the check).
- **Resource types passed to `define`, `check_with` and `authorize_with` implement the new
  `TenantScope` trait.** `Tenanted` types do so already (blanket impl); `()`, `str`, `String` and
  `Acl` have no tenant. For your own resource of no tenant, add `impl TenantScope for Report {}`
  (the error without it: `the trait bound Report: TenantScope is not satisfied`). A type cannot
  implement both `Tenanted` and `TenantScope` by hand.
- **Crossing tenants on purpose** (support tooling, a platform operator):
  `policy.dangerous_cross_tenant("reason")` returns a `CrossTenant` policy that skips the check.
  Make it the outermost call (`a.and(b).dangerous_cross_tenant(..)`, not
  `a.dangerous_cross_tenant(..).and(b)`). It records the posture opt-out
  `id_effect_authz::PolicyExt::dangerous_cross_tenant`, which the prod posture check rejects
  unless accepted.
- `Policy` has a new provided method `crosses_tenants()` (default `false`); implementations need
  no change.

## Processes: `Next`, `Info` and `SendError`

Every process mailbox is bounded now (ADR 0013 R4; finding M10, decision D17).

- **Mailboxes hold at most 10,000 user messages** (`DEFAULT_MAILBOX_CAPACITY`). Set another bound
  per process with `ProcessNode::spawn_with(args, env, SpawnOptions::new().mailbox_capacity(n))`
  or `spawn_linked_to_with`; `Mailbox::new()` is bounded at the default, `Mailbox::with_capacity`
  takes a bound. There is no unbounded mailbox. Exit signals and monitor downs bypass the bound
  (they enter through `Mailbox::offer_system_now`), and a kill never queues.
- **`SendError` is `#[non_exhaustive]` and has a new variant `Full`.** `Addr::cast` and
  `Addr::send` fail with `SendError::Full` when the target's mailbox is full; `SendError::Rejected`
  now means only "mailbox closed" (its message reads `message rejected: mailbox closed`). A
  `match` on `SendError` needs a `Full` arm or a wildcard.
- **`CallError` is `#[non_exhaustive]` and has a new variant `MailboxFull`**: `Addr::call` to a
  full mailbox fails with it instead of enqueueing.
- **`Addr::send_now` and `Mailbox::offer_now` return `false` for a full mailbox** as well as a
  closed one. `Mailbox::try_offer_now` says which (`OfferError::Full` or `OfferError::Closed`).
- **Remote delivery respects the bound** (`id_effect_node`): a remote call to a full mailbox fails
  with the new `RemoteFault::MailboxFull`; a remote cast or info to a full mailbox is dropped and
  counted (`BehaviorRegistry::dropped_full`). A `match` on `RemoteFault` needs the new arm.
- A process that relied on an unbounded queue (a burst producer with no back-pressure) either
  handles `SendError::Full` (retry later, shed, or reply "busy") or spawns with a larger bound.

New: `ProcessCtx::receive::<Self, _>(matcher, timeout)` is a selective receive. It takes the first
message `matcher` accepts and leaves every skipped message queued, in arrival order, for the
normal loop; it fails with `ReceiveError::Timeout` past the deadline, `ReceiveError::ScanLimit`
once a capacity's worth of messages was skipped, and `ReceiveError::Closed` when the process is
being killed.

`Next`, `Info` and `ExitReason` are `#[non_exhaustive]` and gain a variant each (fault.actors #6,
#8; book chapter "Timers, selective receive and hibernate"):

- **`Next::Hibernate(state)`**: the loop runs the new provided `Process::hibernate` hook (default:
  keep the state), releases the mailbox buffer and resumes on the next message. Code that only
  builds `Next::Continue` / `Next::Stop` compiles unchanged; a `match` on a `Next` outside
  `id_effect_process` needs a wildcard arm.
- **`Info::Timeout`**: delivered to `handle_info` by the new idle timeout
  (`ProcessCtx::set_idle_timeout(d)`, once per idle period). It only arrives if you set one, but
  an exhaustive `match` over `Info<_>` no longer compiles (`non-exhaustive patterns: _ not
  covered`): add `_ => {}` or an `Info::Timeout` arm plus a wildcard.
- **`ExitReason::NameConflict(name)`** (`Display`: `name conflict: <name>`): the exit reason of a
  process that loses a cluster-wide name (`id_effect_node::name_conflict`, recognised by
  `conflicting_name`); test for it with `conflicting_name(&reason)` or a `NameConflict` arm, not
  by string. Breaking for 0.10 code: an exhaustive `match` on `ExitReason` needs a wildcard arm.
  On the wire it is the appended `ExitReasonWire` variant `NameConflict` (earlier variants keep
  their tags); a reason a wire version cannot name travels as `Error(<display>)`.

New: `ProcessCtx::send_after::<Self>(msg, d)` and `Addr::send_after(msg, d)` return a
`TimerRef`; `ProcessCtx::cancel_timer(timer)` / `TimerRef::cancel()` returning `true` means the
message is never delivered. Timers run on the executor clock, die with their process and count
against the mailbox bound when they fire (a full mailbox drops them). `Mailbox::allocated()` and
`Mailbox::shrink_to_fit()` expose the buffer that hibernating releases.

## Logging: `RedactionPolicy::new`

Logs are redacted and escaped by default (ADR 0013 R3; findings M11, M12).

- **No policy installed now means `RedactionPolicy::recommended()`**, not "redact nothing". It
  masks the annotation keys `password`, `passwd`, `secret`, `token`, `access_token`,
  `refresh_token`, `id_token`, `api_key`, `apikey`, `authorization`, `cookie`, `set-cookie`,
  `session`, `csrf`, `private_key`, `client_secret` and the globs `*_token`, `*_secret`,
  `*password*` (ASCII case-insensitive). `RedactionPolicy::default()` is the same policy.
- **`RedactionPolicy::new()` is deprecated** (removed in 0.12). It still returns a policy with no
  rules; replace it with:
  - `RedactionPolicy::recommended().field(..)…` to keep the default rules and add yours (an
    installed policy replaces the default, it does not extend it);
  - `RedactionPolicy::empty().field(..)…` to pick every rule yourself (the exact old behaviour);
  - `RedactionPolicy::dangerous_none()` to log in clear on purpose. It is recorded as the posture
    opt-out `id_effect_logger::RedactionPolicy::dangerous_none`, which the prod posture check
    rejects unless accepted.
- `active_redaction_policy()` returns the policy the logger applies (installed or default), and
  `RedactionPolicy::is_empty()` says whether it has any rule.
- **Line formats escape untrusted text.** `TracingLogBackend` writes CR, LF, tab as `\r`, `\n`,
  `\t` and every other control character (ANSI `ESC`, C1, DEL, U+2028/U+2029) as `\u{..}` in the
  message, target, spans, keys and values; spaces, `=`, `"` and `\` in keys become `\u{20}`,
  `\u{3d}`, `\u{22}`, `\u{5c}`; a value that is empty or holds a space, `=` or `"` is written in
  double quotes with `"` and `\` escaped. `StructuredLogBackend` escapes keys the same way (its
  values were already quoted). A log parser that split on raw spaces in keys, or matched raw
  newlines or colour codes in messages, needs updating; the JSON backend is unchanged.

## Config: `Secret<T: Zeroize>` and its equality

Filled by `r11-sec-secrets`.

## STM: `id_effect_fp::stm` moves to `id_effect::stm`

Software transactional memory is a core module again (D5): `id_effect::stm` exposes `Stm`,
`TRef`, `TMap`, `TQueue`, `TSemaphore`, `Txn`, `Outcome`, `commit` and `atomically`. It is always
compiled (no feature, no new dependency) and its semantics are unchanged, including the global
commit lock. The items are not re-exported at the `id_effect` root.

`id_effect_fp::stm::*` and the root re-exports `id_effect_fp::{TRef, commit, …}` still work in
0.11 as `#[deprecated(note = "use id_effect::stm")]` aliases of the core items (the same types,
so values pass freely between old and new paths). They are removed in 0.12.

```rust,ignore
// 0.10
use id_effect_fp::{Stm, TRef, commit};
// 0.11
use id_effect::stm::{Stm, TRef, commit};
```

If `id_effect_fp` was only a dependency for STM, drop it. The STM examples (`077`–`086_stm_*`) and
the contention test moved to `crates/id_effect`: `cargo run -p id_effect --example 077_stm_tref`.

## HTTP client: SSRF guard on by default

Filled by `r11-sec-client-ssrf`.

## Node: plaintext TCP needs the `insecure-plaintext-tcp` feature

Filled by `r11-e8-3-tcp-tls`.

## Web: CSRF masked token format

Filled by `r11-sec-csrf-headers`.

## Node: global names (`ProcessEnvelope` gains variants)

`Mesh::global()` returns the new cluster-wide name registry (`GlobalRegistry`, book chapter
"Cluster-wide names"). Nothing changes unless you match on `ProcessEnvelope` or mix versions:

- `ProcessEnvelope` gains `GlobalRegister`, `GlobalUnregister` and `GlobalSync`. An exhaustive
  `match` over it needs arms for them (or a `_` arm).
- A 0.10 node cannot decode these envelopes. Do not run 0.10 and 0.11 nodes in one mesh; the
  protocol version check (next section) refuses the mix.
- `Mesh::add_peer` now sends the new peer the global names this node owns (a `GlobalSync`), but
  only when it owns at least one. Code that counts envelopes on a peer sender sees it only if
  it registers global names.

## Node: mesh protocol version

Filled by `r11-e8-3-reliable-delivery`.

## Workflows: `id_effect_workflow` replaces the labs crate

`id_effect_workflow` is a new published crate (tier integration): durable workflows journaled in
Postgres through `id_effect_sql` (decisions D6, D7). Nothing published before 0.11 changes; this
section is for users of the unpublished `labs/id_effect_workflow`.

- **No duroxide, duroxide-pg, SQLite or `NetworkJournalStub`.** The store is `PgWorkflowStore`
  over any `SqlClient` (tables `workflow_instances`, `workflow_history`); run
  `id_effect_workflow::migrator()` at startup (it tracks its versions in
  `_id_effect_workflow_migrations`) or copy `migrations/0001_id_effect_workflow.{up,down}.sql` into
  your own migrations. `MemoryWorkflowStore` replaces the in-memory journals in tests. Journals
  written by the labs crate are not read; finish or drop those instances before switching.
- **A workflow is a type implementing `Workflow`** (`NAME`, `VERSION`, schema-encoded `Input` and
  `Output`, `run(ctx, input)`), registered with `WorkflowEngine::register` and started with
  `engine.start::<W>(WorkflowId, input)` (idempotent per id). Read it back with `status`, `result`
  and `instance`; drive it with `engine.run()` (forked; any number of engines per database),
  `run_once` or `resume`.
- **Side effects go through `ctx.step(name, effect)`**, time through `ctx.now()` and randomness
  through `ctx.random()`; each is journaled and replayed by position and name. A replay that asks
  for another step than the history holds fails the instance with
  `WorkflowError::NonDeterministic`, so renaming or reordering steps of a running workflow needs a
  new workflow name until versioned routing ships (E13.5). Step results must be `#[derive(Schema)]`
  types (`HasSchema<A = Self, I = Unknown, E = ()>`), like job payloads.
- **Activities are `id_effect_jobs` jobs.** Implement `Activity` (`NAME`, `Input`, `Output`,
  `run(ctx, input) -> Effect<Output, JobFailure>`, optional `QUEUE`, `MAX_ATTEMPTS`, `TIMEOUT`,
  `retry()`), call it from a body with `ctx.activity::<A>(input)`, give the engines a queue with
  `WorkflowEngine::with_job_queue(PgJobQueue)`, and add the handlers to the workers' registry with
  `engine.register_activity(registry, activity)`; the workers serve `ACTIVITY_QUEUE` (or the
  activity's `QUEUE`). The labs crate's in-process activity calls have no equivalent: a side
  effect is a `ctx.step` (in the engine) or an activity (on any worker).
- **Durable timers** are `ctx.sleep(d)` / `ctx.sleep_until(t)`: one job on `TIMER_QUEUE` with
  `run_at`; add `engine.register_timers(registry)` and serve `TIMER_QUEUE` on the workers.
- **Signals**: `engine.client().signal(&id, name, payload)` (or `WorkflowClient::new(store)`) and
  `ctx.wait_signal::<P>(name)` in the body; signals sent before the wait are buffered.
- Run the crate's `migrator()` again: migration `0002_activities_timers_signals` adds
  `workflow_instances.waiting` / `wakeups`, `workflow_completions` and `workflow_signals`, and
  replaces the claim index. Run `id_effect_jobs::migrator()` too (the job table). Custom
  `WorkflowStore` implementations gain `wait`, `completion`, `complete`, `signal` and
  `next_signal`; `InstanceRecord` gains `waiting` and `wakeups`, `Claimed` gains `wakeups`.
- A body that catches `WorkflowError` must let `Waiting` (the run parked) and `LeaseLost` through;
  a caught `NonDeterministic` still fails the instance.

## Lints: `id_effect_lints` replaces the labs crate

The unpublished `labs/id_effect_lint` is now the Dylint library `id_effect_lints` at
`lints/id_effect_lints` (E1.9). Nothing published before 0.11 changes; this section is for users
of the labs crate.

- **Run it with `cargo dylint`** instead of loading the cdylib into a rustc driver yourself:
  `cargo install cargo-dylint dylint-link`, then name it in `[workspace.metadata.dylint]`
  (`{ git = "https://github.com/Industrial/id_effect", tag = "v0.11.0", pattern =
  "lints/id_effect_lints" }`) and run `cargo dylint --all`. Dylint builds it with the nightly in
  its `rust-toolchain.toml`; your toolchain does not change. Book appendix D has the details.
- **Every lint warns.** The labs crate declared 16 of them `Deny`; raise the ones you relied on
  with `DYLINT_RUSTFLAGS="-D <lint>"` or `#![cfg_attr(dylint_lib = "id_effect_lints",
  deny(<lint>))]`. A plain `#[allow(<lint>)]` is an unknown lint to ordinary `rustc`; wrap it the
  same way.
- **Detection fixes:** `#[test]` functions and `#[cfg(test)]` items are recognised (on current
  nightlies the labs crate's attribute lookup found neither, so the test-only lints never fired
  and the test exemptions never applied);
  `panic!`/`unreachable!`/`println!` in Effect code, `SystemTime::now()`, `#[thread_local]` and
  `thread_local!` are reported; `.await` inside `from_async(…)` and `static NAME: &str` are no
  longer reported. Expect new warnings where those lints were silent.
- **New lints:** `effect_map_identity`, `effect_flat_map_succeed` and `effect_flat_map_then`
  (fixed by `cargo dylint --fix`), and `raw_sql_non_literal` (see "SQL: `Sql` and `RawSql`").
- The ast-grep rules moved unchanged: `ast-grep scan --config lints/id_effect_lints/sgconfig.yml`.
