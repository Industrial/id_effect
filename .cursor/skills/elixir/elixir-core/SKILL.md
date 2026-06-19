---
name: elixir-core
description: >-
  Applies idiomatic Elixir — pattern matching, pipelines, Enum/Stream, protocols,
  structs, and @spec. Use when editing .ex/.exs files, writing pure functions,
  refactoring imperative code, or when the user asks about Elixir language idioms.
---

# Elixir Core

Language idioms from *Programming Elixir* and *Learn Functional Programming with Elixir*. Prefer pure functions and explicit data transformation over hidden mutation.

## Before writing code

1. Can this be a pure function? → No process needed.
2. Is the data shape wrong? → Fix the struct/map first, not the algorithm.
3. Would pattern matching clarify control flow? → Use it instead of `if`/`case` on booleans.

## Pattern matching

- Match on **structure**, not sentinel values when possible.
- Use `=` for binding, `<-` in `with`/`for` for match-or-fail.
- Pin `^var` when rebinding must not shadow.
- Function heads over nested `case` inside one function.

```elixir
# Prefer
def handle({:ok, value}), do: ...
def handle({:error, reason}), do: ...

# Over
def handle(result) do
  case result do
    {:ok, v} -> ...
    {:error, r} -> ...
  end
end
```

## Pipelines and data

- `|>` threads the first argument; use `then/2` when the value is not first.
- `Enum` for eager; `Stream` for lazy or large collections.
- Prefer `map` + `filter` as one pass (`flat_map`, `reduce`, or `for`) when both apply.

## Error handling

| Situation | Pattern |
|-----------|---------|
| Expected failure in business logic | `{:ok, _}` / `{:error, _}` tuples |
| Chain of fallible steps | `with` |
| Programmer bug / invariant | `raise` or `!/1` variants |
| Recoverable in caller | Return tagged tuple, don't rescue broadly |

Avoid `try/rescue` for control flow. Rescue only at boundaries (HTTP, CLI, OTP callbacks).

## Structs and maps

- Use `%Module{}` structs at domain boundaries; plain maps for internal transforms if ephemeral.
- Access struct fields with `.` when the type is known; `Map.get/2` for dynamic keys.
- `@enforce_keys` and `@derive` when the struct is part of a public API.

## Protocols vs behaviour

- **Protocol** — open extension across types (`Enumerable`, custom).
- **Behaviour** — closed contract for OTP callbacks or internal adapters.
- Don't protocol-ify a single implementation.

## Typespecs

Add `@spec` on public functions in libraries and contexts. Match arity and tagged return types:

```elixir
@spec fetch(id()) :: {:ok, User.t()} | {:error, :not_found}
```

Use `@type` / `@opaque` for domain types. Run `mix dialyzer` when the project configures it.

## Anti-patterns

See [reference/anti-patterns.md](reference/anti-patterns.md).

## Additional resources

- Idiom catalog: [reference/idioms.md](reference/idioms.md)
