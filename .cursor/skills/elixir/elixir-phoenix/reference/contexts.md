# Phoenix Contexts

## Structure

```
lib/my_app/
  accounts.ex              # context API
  accounts/
    user.ex                # schema + changeset
    user_token.ex
lib/my_app_web/
  controllers/
    user_controller.ex
```

## Context API design

```elixir
defmodule MyApp.Accounts do
  alias MyApp.Accounts.User
  alias MyApp.Repo

  def get_user!(id), do: Repo.get!(User, id)

  def create_user(attrs) do
    %User{}
    |> User.registration_changeset(attrs)
    |> Repo.insert()
  end

  def change_user(%User{} = user, attrs \\ %{}) do
    User.registration_changeset(user, attrs)
  end
end
```

## Changesets

- **Cast + validate_required + validate_format** — structural validation on schema.
- **Business rules** — context functions before insert, or `Ecto.Changeset.validate_change/3` when tied to DB constraints.
- **`Ecto.Multi`** — multi-table writes with single transaction boundary.

```elixir
Ecto.Multi.new()
|> Ecto.Multi.insert(:user, changeset)
|> Ecto.Multi.insert(:profile, fn %{user: user} -> profile_cs(user) end)
|> Repo.transaction()
```

## Boundary vs core within context

Extract pure functions when rules grow:

```elixir
defp eligible_for_discount?(user, order) do
  Billing.discount_eligible?(user.tier, order.total)
end
```

`Billing` has no Repo — test without database.

## Authorization

- Bodyguard, Permit, or explicit `Accounts.authorize/2` in context.
- Check policy in context or plug, not scattered in templates.

## Testing contexts

```elixir
test "create_user/1 with valid data" do
  assert {:ok, %User{} = user} = Accounts.create_user(valid_attrs())
  assert user.email == "a@b.com"
end
```

Use `MyApp.DataCase` with SQL sandbox; no mocks for Repo in context tests.
