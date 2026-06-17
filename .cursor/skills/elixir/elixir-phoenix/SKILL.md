---
name: elixir-phoenix
description: >-
  Builds Phoenix web applications using contexts, plugs, routers, Ecto, and
  channels. Use when editing Phoenix controllers, contexts, schemas, plugs,
  router, or Ecto queries outside of LiveView-specific code.
---

# Elixir Phoenix

From *Programming Phoenix* and Phoenix guides. Web layer stays thin; contexts own business logic.

## Context-first rule

Controllers, channels, and JSON APIs call **context** functions:

```elixir
# Controller — thin
def create(conn, %{"user" => params}) do
  case Accounts.create_user(params) do
    {:ok, user} -> redirect(conn, to: ~p"/users/#{user}")
    {:error, changeset} -> render(conn, :new, changeset: changeset)
  end
end
```

No `Repo` in controllers. No business rules in changesets beyond data shape validation.

## Context module checklist

- [ ] Public API is small and named after domain verbs (`create_user`, `list_posts`).
- [ ] Schemas are boundary types — changesets live with schema or context.
- [ ] Multi-step operations use `Ecto.Multi` or `Repo.transaction`.
- [ ] Context returns `{:ok, _}` / `{:error, changeset | reason}`.

See [reference/contexts.md](reference/contexts.md).

## Router and plugs

- Use `pipe_through` for shared stacks (`:browser`, `:api`).
- Authentication plug assigns `current_user` — never fetch user in every action manually.
- Scope routes by resource; prefer `resources/2` and `live/3` (LiveView in sibling skill).

## Plugs

- `halt/1` after sending response on auth failure.
- `conn.assigns` for request-scoped data only.
- Heavy work belongs in contexts, not plugs.

## Ecto

See [reference/ecto.md](reference/ecto.md).

- Preload associations explicitly — avoid N+1.
- `Ecto.Query` composable functions in schema or query module.
- `Repo.get` / `Repo.get!` vs `Repo.one` — prefer `get_by` with tags for APIs.

## Channels (non-LiveView)

- Channel is transport; delegate to context on join and handle_in.
- Use PubSub for fan-out; channel pushes presentation, not domain logic.
- Test channels with `MyAppWeb.ChannelCase`.

## JSON / API

- Separate fallback controller or action clause for errors.
- Consistent JSON shape `%{errors: ...}` or JSON:API if project uses it.
- Use `OpenApiSpex` or similar when project already has spec-driven APIs.

## Configuration

- `config/runtime.exs` for env-specific release config.
- Don't call `Application.get_env` deep in domain — pass config at boundary.

## Verification

```bash
mix format --check-formatted
mix test
mix phx.routes    # inspect routing
```

## Anti-patterns

| Avoid | Prefer |
|-------|--------|
| Fat controllers | Context + thin controller |
| Changeset.validate_* for business rules | Context validation + changeset for types |
| `Repo.all` unbounded | Pagination, streams |
| Logic duplicated across HTML and JSON | Shared context functions |
