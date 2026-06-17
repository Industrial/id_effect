# LiveView Components

## Function components (Phoenix 1.7+)

```elixir
def user_card(assigns) do
  ~H"""
  <div id={"user-#{@user.id}"}>
    <%= @user.name %>
  </div>
  """
end
```

- Declare `attr/3` for required/optional attrs.
- Use slots for flexible composition.

## Stateful LiveComponent

```elixir
defmodule MyAppWeb.CounterComponent do
  use MyAppWeb, :live_component

  def render(assigns) do
    ~H"""
    <div>
      <%= @count %>
      <button phx-click="inc" phx-target={@myself}>+</button>
    </div>
    """
  end

  def handle_event("inc", _, socket) do
    {:noreply, assign(socket, count: socket.assigns.count + 1)}
  end
end
```

Parent invokes: `<.live_component module={CounterComponent} id="counter" count={0} />`

## When to extract a component

| Signal | Action |
|--------|--------|
| Same markup in 2+ LiveViews | Function component |
| Isolated state + events | Stateful LiveComponent |
| Single LiveView > ~200 lines HEEx | Extract function components |

## Layouts and root

- `Layouts.app` wrapper for flash, nav, assets.
- `@flash` from `put_flash`; `@current_scope` / `@current_user` from plug or `on_mount`.

## `on_mount` hooks

- Authentication and default assigns shared across LiveViews.
- Keep hooks thin — call context, assign results.

## Streams + components

Each stream item needs stable DOM id:

```heex
<div id={@streams.items}-wrapper phx-update="stream">
  <div :for={{id, item} <- @streams.items} id={id}>
    <.item_card item={item} />
  </div>
</div>
```

## Uploads

- `allow_upload/3` in mount when connected.
- Consume entries in event handler; progress via `@uploads`.
