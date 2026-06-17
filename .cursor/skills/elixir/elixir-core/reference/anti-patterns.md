# Elixir Core — Anti-patterns

## Language

| Avoid | Prefer |
|-------|--------|
| `Enum.each` for transformation | `Enum.map` / `for` with `:into` |
| Nested `case` three levels deep | Multiple function heads or `with` |
| `String.to_atom/1` on user input | Existing atoms or string keys |
| `:timer.sleep` in tests | `Process.sleep` with explicit comment, or sync messages |
| Large literal lists in module body | Module attribute or function |
| `Application.get_env` in pure domain | Pass config as argument from boundary |

## Style

| Avoid | Prefer |
|-------|--------|
| Comments describing what code does | Clear names; comment why |
| `defmodule` per tiny helper | Colocate or private functions |
| `@doc` on every private function | `@doc false` or omit |
| Catching `:exit` broadly | Let Supervisor handle; narrow rescue at edge |

## Performance (premature unless profiled)

| Avoid | Prefer |
|-------|--------|
| Manual recursion over `Enum` | Built-ins until proven slow |
| `:binary.copy` micro-opts | Measure first |
| Spawning a process per item by default | `Task.async_stream` with `max_concurrency` |

## OO habits

| Avoid | Prefer |
|-------|--------|
| God struct with many optional fields | Separate structs or tagged variants |
| Boolean flags encoding state | Atoms or separate types |
| Mutating in place mental model | Bind new values; `put_in`/`update_in` for nested immutability |
