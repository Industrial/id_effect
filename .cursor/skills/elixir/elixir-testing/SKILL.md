---
name: elixir-testing
description: >-
  Writes ExUnit tests for Elixir and Phoenix using context tests, LiveView tests,
  and property-based testing patterns. Use when editing test/ files, adding test
  coverage, or when the user asks how to test Elixir code.
---

# Elixir Testing

ExUnit + community patterns (StreamData, Mox at boundaries). Test behavior, not implementation.

## Test layout

```
test/
  my_app/           # context/unit
  my_app_web/       # controllers, LiveView, channels
  support/
    data_case.ex
    conn_case.ex
```

## ExUnit basics

```elixir
defmodule MyApp.AccountsTest do
  use MyApp.DataCase, async: true

  alias MyApp.Accounts

  describe "create_user/1" do
    test "creates with valid attrs" do
      assert {:ok, user} = Accounts.create_user(valid_user_attrs())
      assert user.email == "u@example.com"
    end

    test "returns error with invalid email" do
      assert {:error, changeset} = Accounts.create_user(%{email: "bad"})
      assert "has invalid format" in errors_on(changeset).email
    end
  end
end
```

- `describe` groups related tests.
- `async: true` when safe (no shared DB state, no global process names).
- `setup` / `setup tags` for fixtures.

## What to test where

| Layer | Case module | Focus |
|-------|-------------|-------|
| Context / domain | `DataCase` | Public API, DB side effects |
| Controller | `ConnCase` | Status, redirect, flash |
| LiveView | `ConnCase` + `LiveViewTest` | Events, rendered HTML |
| JSON API | `ConnCase` | Response body, status |
| Pure functions | Plain `ExUnit.Case` | No DB |

## DataCase / SQL sandbox

```elixir
setup tags do
  pid = Ecto.Adapters.SQL.Sandbox.start_owner!(MyApp.Repo, shared: not tags[:async])
  on_exit(fn -> Ecto.Adapters.SQL.Sandbox.stop_owner(pid) end)
  :ok
end
```

## Fixtures

- `ExMachina` or explicit factory functions in `test/support/fixtures/`.
- Minimal attrs builders: `valid_user_attrs/0` over giant fixtures.

## Mocking

- **Prefer real implementations** with sandbox DB and PubSub test adapter.
- **Mox** only for external HTTP, time, or irreversible side effects.
- Define behaviour + mock for port; don't mock internal modules.

```elixir
# config/test.exs
config :my_app, HTTPClient, MyApp.HTTPClientMock
```

## LiveView tests

```elixir
test "creates item", %{conn: conn} do
  {:ok, lv, _html} = live(conn, ~p"/items/new")
  lv |> form("#item-form", item: %{name: "Test"}) |> render_submit()
  assert_redirect(lv, ~p"/items")
end
```

Assert on **visible text and redirects**, not internal assigns.

## Property-based (StreamData)

Use for parsers, encoders, pure transforms:

```elixir
property "encode/decode roundtrip" do
  check all data <- string(:alphanumeric) do
    assert decode(encode(data)) == data
  end
end
```

## Doctests

- Stable, simple examples only.
- Run via `doctest MyModule` in test file or module test.

## Tagging

```elixir
@tag :integration
@tag :skip
@tag timeout: 60_000
```

## Verification commands

```bash
mix test
mix test --failed
mix test test/my_app/accounts_test.exs:42
mix test --cover
```

## Anti-patterns

| Avoid | Prefer |
|-------|--------|
| Testing private functions | Public API |
| `Process.sleep` for sync | `assert_receive` or LiveView helpers |
| Mocking Repo | Sandbox + real inserts |
| One giant test | Many focused tests in `describe` |
| Testing implementation details | Observable outcomes |

## Additional resources

- Patterns: [reference/patterns.md](reference/patterns.md)
