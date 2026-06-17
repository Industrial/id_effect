# Elixir Core — Idioms

## `with` for happy path

```elixir
with {:ok, user} <- Accounts.get_user(id),
     {:ok, order} <- Orders.create(user, attrs) do
  {:ok, order}
else
  {:error, :not_found} -> {:error, :user_not_found}
  {:error, reason} -> {:error, reason}
end
```

## `cond` vs multiple clauses

- Multiple function clauses → dispatch on shape/variant.
- `cond` → three or more unrelated boolean guards in one function.

## Default args and keyword options

```elixir
def list(opts \\ []) do
  limit = Keyword.get(opts, :limit, 100)
  ...
end
```

Prefer keyword lists for optional config; maps for data records.

## Comprehensions

Use `for` when filtering + mapping + `:into` collection building is clearer than nested `Enum`.

## Documentation

- `@moduledoc` and `@doc` on public modules and functions.
- Examples in `@doc` with `# iex>` doctests when stable.
- `@doc false` for internal callbacks, not deletion of docs.

## Naming

- `snake_case` functions and variables.
- `CamelCase` modules.
- Predicates: `is_*` guards only in guards; functions named `*_?` are not special in Elixir — use `valid?/1` as a regular function.
- Bang functions (`save!`) raise; non-bang return tuples.

## Modules

- One main concept per module; nest with `Parent.Child` for sub-concepts.
- `@moduledoc false` for internal implementation modules.
- Avoid circular aliases — extract shared types to a small module.
