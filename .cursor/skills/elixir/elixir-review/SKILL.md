---
name: elixir-review
description: >-
  Reviews Elixir and Phoenix code for idiomaticity, layer violations, OTP misuse,
  and test gaps. Use when reviewing pull requests, examining .ex changes, or
  when the user asks for an Elixir code review.
---

# Elixir Review

Synthesis gate across all Elixir skills. Read changed files, then apply this checklist.

## Review workflow

1. Identify **layer** of each changed module (core, context, web, OTP).
2. Check **public API** surface — new functions need specs and tests.
3. Run mental `mix test` — are error paths covered?
4. Classify findings (below).

## Severity

| Level | Meaning |
|-------|---------|
| **Critical** | Must fix — bugs, layer breaks, OTP crash risk, security |
| **Suggestion** | Should fix — idioms, maintainability, missing tests |
| **Nice** | Optional — naming, docs, micro-style |

## Layer violations (Critical)

- [ ] `Repo` or `Ecto.Query` in controller / LiveView / channel
- [ ] Phoenix / Plug imports in domain core
- [ ] Business rules only in HEEx template
- [ ] `Application.get_env` in pure domain functions

## OTP (Critical / Suggestion)

- [ ] GenServer state bounded — not an in-memory DB
- [ ] Supervision strategy matches failure domain
- [ ] Public GenServer API via facade module
- [ ] No blocking work in `init/1`

## Idioms (Suggestion)

- [ ] Tagged tuples for expected errors
- [ ] Pattern matching over boolean soup
- [ ] `@spec` on new public functions
- [ ] No `String.to_atom/1` on user input

## Phoenix / LiveView (Critical / Suggestion)

- [ ] Controller/LiveView thin — logic in context
- [ ] Changesets for structural validation only
- [ ] N+1 queries — preloads present
- [ ] LiveView lists use streams when dynamic/large
- [ ] PubSub subscribe gated on `connected?/1`

## Concurrency (Critical)

- [ ] `Task.async_stream` has `max_concurrency` and `timeout`
- [ ] Pipeline concurrency respects DB pool / API limits
- [ ] No unbounded spawns

## Tests (Suggestion / Critical)

- [ ] New context functions have tests
- [ ] LiveView happy path tested for UI changes
- [ ] `async: true` safe — no shared global state
- [ ] Mox only at true boundaries

## Security (Critical)

- [ ] User input validated at boundary
- [ ] Authorization checked before mutation
- [ ] No secrets in logs or assigns sent to client

## Output format

```markdown
## Elixir Review — [brief scope]

### Critical
- `path:line` — issue and suggested fix

### Suggestions
- `path:line` — issue and suggested fix

### Nice
- optional notes

### Summary
One paragraph: merge readiness and main risk.
```

## Verification to request

```bash
mix format --check-formatted
mix test
mix doctor --raise --summary
mix docs --warnings-as-errors
mix credo --strict    # if project uses Credo
```

## Additional resources

- Full checklist: [reference/checklist.md](reference/checklist.md)

Cross-reference skills: `elixir-core`, `elixir-otp-design`, `elixir-phoenix`, `elixir-liveview`, `elixir-concurrency`, `elixir-testing`.
