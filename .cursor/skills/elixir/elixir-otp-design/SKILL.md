---
name: elixir-otp-design
description: >-
  Designs Elixir OTP systems using layered boundaries, supervision, and fault
  tolerance. Use when implementing GenServer, Agent, Supervisor, Application,
  designing process trees, or when the user mentions OTP, processes, layers, or
  fault tolerance.
---

# Elixir OTP Design

Combines *Designing Elixir Systems with OTP* (layering, APIs) with *Elixir in Action* (processes, supervision, releases).

## Process selection

| Need | Use |
|------|-----|
| Sync call + mutable process state | `GenServer` |
| Simple key-value, no custom logic | `Agent` (rare — prefer `GenServer` or ETS) |
| One-off async work | `Task` / `Task.Supervisor` |
| Shared concurrent cache | `ETS` + optional `GenServer` gate |
| Coordinate many workers | `DynamicSupervisor` + worker module |

**Do not** use a GenServer as a database. Large collections belong in ETS, the database, or streaming.

## Layer rules

Build inside-out. See [reference/layers.md](reference/layers.md).

1. **Core** — pure domain logic; no Phoenix, Ecto, HTTP, or `Application.get_env/2`.
2. **Boundary** — translate external representations ↔ domain; validate input here.
3. **Service** — orchestrate core + boundaries; optional GenServer for coordination.
4. **API / Web** — Phoenix, plugs, channels, LiveView call into contexts, not raw Repo.

Dependencies point **inward**. Core never imports outer layers.

## GenServer checklist

- [ ] State is minimal (IDs, counters, small structs — not full tables).
- [ ] `handle_call` for sync; `handle_cast` only when fire-and-forget is safe.
- [ ] `handle_info` for timeouts and parent/down messages — document each clause.
- [ ] `init/1` returns `{:ok, state}` or `{:ok, state, timeout}` — no heavy work; use `continue` or cast to self.
- [ ] Public API is a **facade module** (`MyApp.Worker`) — callers never `GenServer.call` the module directly if avoidable.

## Supervision

See [reference/supervision.md](reference/supervision.md).

- One failure domain per supervisor subtree.
- Choose strategy deliberately: `:one_for_one` (default), `:rest_for_one`, `:one_for_all`.
- Restart intensity: `max_restarts` / `max_seconds` prevent infinite crash loops.
- **`Supervisor.start_link`** in Application; children are `{Module, arg}` or `%{}` spec maps.

## Application callback

```elixir
def start(_type, _args) do
  children = [
    MyApp.Repo,
    {MyApp.PubSub, name: MyApp.PubSub},
    MyApp.Supervisor
  ]

  Supervisor.start_link(children, strategy: :one_for_one, name: MyApp.Supervisor)
end
```

Order children so dependencies start first. Use `start_phases` only when needed.

## Naming and registration

- Prefer `name: __MODULE__` or `via: Registry` over global names.
- `Registry` for dynamic process lookup by key.

## Testing OTP

- Test public facade API, not GenServer internals.
- Use synchronous calls in tests; trap exits only when testing crash behavior.
- `start_supervised!/1` in ExUnit when available.

## Anti-patterns

| Avoid | Prefer |
|-------|--------|
| GenServer storing unbounded lists/maps | ETS + eviction, or DB |
| Supervise every Task individually | `Task.Supervisor` |
| Business logic in `init/1` | Fast init; defer work |
| Calling `GenServer.call` from inside same server | Direct state update |
| `use GenServer` in web layer | Context + service layer |
