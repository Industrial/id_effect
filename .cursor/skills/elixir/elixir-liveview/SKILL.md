---
name: elixir-liveview
description: >-
  Builds Phoenix LiveView interfaces with correct lifecycle, components, streams,
  and PubSub patterns. Use when editing .ex files in live/ directories, HEEx
  templates, LiveComponents, or when the user mentions LiveView, assigns, or
  handle_event.
---

# Elixir LiveView

From *Programming Phoenix LiveView*. LiveView is UI transport — not a place for business logic.

## Golden rule

**LiveView orchestrates; context executes.**

```elixir
def handle_event("save", %{"user" => params}, socket) do
  case Accounts.update_user(socket.assigns.user, params) do
    {:ok, user} ->
      {:noreply, assign(socket, user: user) |> put_flash(:info, "Saved")}

    {:error, changeset} ->
      {:noreply, assign(socket, changeset: changeset)}
  end
end
```

## Lifecycle

| Callback | Use |
|----------|-----|
| `mount/3` | Load assigns; subscribe to PubSub; `connected?(socket)` gate for one-time subscribe |
| `handle_params/3` | URL-driven state (tabs, filters, pagination) |
| `handle_event/3` | User interactions from `phx-click`, forms |
| `handle_info/2` | PubSub broadcasts, parent messages, timers |
| `terminate/2` | Cleanup rarely needed if linked processes supervised elsewhere |

Subscribe only when `connected?(socket)` to avoid duplicate subscriptions in dead render.

## Assigns discipline

- Keep assigns **minimal** — IDs and data for render, not entire app state.
- `@my_assign` in HEEx for static access; `assigns.user` when needed.
- Don't mutate — always `assign/3` or `assign(socket, kw)`.

## Forms

- Prefer `to_form/2` and `@form` — Phoenix 1.7+ form API.
- `phx-change` for live validation; `phx-submit` for save.
- Errors from changeset, not hand-built error maps.

## Streams (collections)

Use `stream/3` and `@streams` for dynamic lists — avoids full-list re-render and O(n) assign copies:

```elixir
def mount(_, _, socket) do
  {:ok, stream(socket, :items, list_items())}
end

def handle_event("delete", %{"id" => id}, socket) do
  {:noreply, stream_delete(socket, :items, item)}
end
```

See [reference/components.md](reference/components.md) for component boundaries.

## LiveComponents

| Type | When |
|------|------|
| **Stateful** | Encapsulated state + events (`MyAppWeb.UserFormComponent`) |
| **Stateless** | Pure function of assigns passed from parent |

- Pass data **down**, events **up** via `send/2` to parent or `phx-target`.
- Don't call context from stateless component — parent handles events.

## PubSub in LiveView

```elixir
if connected?(socket) do
  Phoenix.PubSub.subscribe(MyApp.PubSub, "room:#{room_id}")
end

def handle_info({:new_message, msg}, socket) do
  {:noreply, stream_insert(socket, :messages, msg, at: 0)}
end
```

Topic naming: `"domain:resource:id"`.

## Performance

- `phx-update="stream"` for lists; avoid huge single templates.
- Debounce inputs: `phx-debounce="300"`.
- `temporary_assigns` or streams for large content that doesn't need diff.
- Move heavy computation to context or async assign (`assign_async/3`).

## JS hooks

Use sparingly for DOM APIs LiveView can't reach. Hook pushes events to LiveView; don't duplicate business logic in JS.

## Testing

```elixir
{:ok, view, _html} = live(conn, ~p"/users")
view |> element("#save") |> render_click()
assert render(view) =~ "Saved"
```

Use `Phoenix.LiveViewTest`; test through user-visible events.

## Anti-patterns

| Avoid | Prefer |
|-------|--------|
| Repo calls in LiveView | Context functions |
| Giant `mount/3` loading everything | `handle_params`, lazy load |
| Full list in assign for 1000+ rows | Streams |
| `send(self(), ...)` for every UI tick | Debounce, batch PubSub |
| Business logic in HEEx | Precompute in mount/event |
