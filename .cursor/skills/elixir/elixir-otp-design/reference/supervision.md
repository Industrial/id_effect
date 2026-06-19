# Supervision Reference

From *Elixir in Action* and OTP design principles.

## Strategies

| Strategy | When |
|----------|------|
| `:one_for_one` | Independent siblings; one crash doesn't affect others |
| `:rest_for_one` | Ordered pipeline — later children depend on earlier |
| `:one_for_all` | Tightly coupled group must restart together (use sparingly) |

## Child specs

```elixir
# Tuple form
{MyApp.Worker, arg}

# Map form (preferred for clarity)
%{
  id: MyApp.Worker,
  start: {MyApp.Worker, :start_link, [arg]},
  restart: :permanent,
  shutdown: 5000,
  type: :worker
}
```

| `restart` | Meaning |
|-----------|---------|
| `:permanent` | Always restart (default for workers) |
| `:temporary` | Never restart |
| `:transient` | Restart only on abnormal exit |

## DynamicSupervisor

For runtime-created workers (connections, per-tenant processes):

```elixir
DynamicSupervisor.start_child(MyApp.DynamicSup, {MyApp.Worker, opts})
```

Pair with `Registry` for lookup. Supervise the DynamicSupervisor, not each dynamic child at top level.

## Fault tolerance mindset

- **Let it crash** — recover via supervision, not defensive try/rescue in every function.
- Idempotent `init/1` and restart-safe state (reload from DB/ETS on start).
- Circuit-break long external calls with `Task` timeouts or dedicated circuit breaker libs.

## Hot code upgrades

Most Elixir apps use rolling deploys, not hot upgrades. Design for **restart recovery**, not `:code_change` unless you explicitly maintain it.

## Observability hooks

- `:telemetry` events at service boundaries (start/stop/exception).
- Attach handlers in Application or a dedicated telemetry module — not scattered `IO.inspect`.
