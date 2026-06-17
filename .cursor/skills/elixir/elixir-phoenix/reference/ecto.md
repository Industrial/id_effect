# Ecto Patterns

## Queries

Compose queries as functions:

```elixir
def active(query \\ User) do
  from u in query, where: u.active == true
end

def with_posts(query \\ User) do
  from u in query, preload: :posts
end
```

## Avoid N+1

```elixir
# Bad — N+1 in template
users = Repo.all(User)
# each user.posts triggers query

# Good
User |> with_posts() |> Repo.all()
```

Or `Repo.preload(users, :posts)` after fetch.

## Pagination

Use `limit/offset` or keyset pagination for large tables. Never `Repo.all` without bound on user-facing lists.

## Changesets and constraints

```elixir
|> unique_constraint(:email)
|> foreign_key_constraint(:user_id)
```

Match DB constraints; let DB enforce uniqueness on race.

## Migrations

- reversible when practical (`change/0` with `execute` pairs).
- indexes on foreign keys and filter columns.
- `timestamps()` on schemas that track rows.

## Sandbox in tests

```elixir
setup tags do
  MyApp.DataCase.setup_sandbox(tags)
  :ok
end
```

`async: true` only when tests don't share global state or named processes.

## Schemas vs embedded

- `@primary_key` and `@foreign_key_type` explicit when using UUIDs.
- `embedded_schema` for value objects without own table.
