# Testing Patterns

## errors_on helper (DataCase)

```elixir
def errors_on(changeset) do
  Ecto.Changeset.traverse_errors(changeset, fn {msg, opts} ->
    Regex.replace(~r"%{(\w+)}", msg, fn _, key ->
      opts |> Keyword.get(String.to_existing_atom(key), key) |> to_string()
    end)
  end)
end
```

## ConnCase authentication

```elixir
setup %{conn: conn} do
  user = user_fixture()
  conn = log_in_user(conn, user)
  {:ok, conn: conn, user: user}
end
```

## Testing PubSub

Use `Phoenix.PubSub` test adapter (default in `config/test.exs`). Subscribe in test process, trigger action, `assert_receive`.

## Testing GenServer

Prefer testing facade module. For crash behavior:

```elixir
Process.flag(:trap_exit, true)
assert {:EXIT, pid, reason} = catch_exit(GenServer.call(pid, :boom))
```

## Flaky test fixes

1. Remove timing dependence — sync on messages.
2. Unique emails/keys per test (`System.unique_integer()`).
3. Don't share named GenServers across async tests.

## Coverage targets

- Context public functions: happy + key error paths.
- LiveView: primary user flows.
- Skip coverage hunting on trivial `@doc` modules.

## CI-friendly

```bash
mix format --check-formatted
mix credo --strict          # if configured
mix test --warnings-as-errors
```
