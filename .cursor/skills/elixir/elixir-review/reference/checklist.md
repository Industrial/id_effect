# Elixir Review — Full Checklist

## Core / language

- Pure functions testable without DB
- `with` chains have `else` when error mapping matters
- Bang vs non-bang consistent with raise policy
- Struct updates use `struct` / `Map.put` / `%{model | field: val}` correctly

## OTP design

- Process per concern — not one god GenServer
- DynamicSupervisor for dynamic children
- Registry for lookup where names are dynamic
- `:telemetry` at service boundaries for new side effects
- Restart-safe state after crash

## Ecto / data

- Migrations reversible or documented
- Indexes on FKs and filtered columns
- `foreign_key_constraint` / `unique_constraint` in changesets
- Pagination on user-facing lists
- Transactions for multi-write operations

## Phoenix web

- Routes RESTful or intentionally not — documented
- Plugs ordered correctly (fetch session before auth)
- CSRF on browser pipelines
- Flash messages on success/failure

## LiveView

- `phx-hook` justified and minimal
- Forms use `to_form`
- Keys stable in `:for` collections
- `temporary_assigns` or streams for large content

## Concurrency

- Broadway ack/retry strategy defined for message pipelines
- Flow partition key correct for reduce semantics
- No shared mutable state between tasks

## Testing

- Factories/fixtures DRY
- Tests independent — order doesn't matter
- Descriptive test names (`test "returns error when email taken"`)
- No `@tag :skip` left without comment

## Docs

- `@moduledoc` on new public modules
- `@doc` with args explained on non-obvious public functions
- README updated if public API or setup changed

## Release / ops (when applicable)

- `runtime.exs` for prod secrets
- Health check endpoint if new critical dependency
- Oban/S Broadway queue config documented
