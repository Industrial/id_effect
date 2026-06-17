# Elixir Cursor Skills

Project skills grounded in the Elixir canon (*Programming Elixir*, *Elixir in Action*, *Designing Elixir Systems with OTP*, *Concurrent Data Processing in Elixir*, *Programming Phoenix*, *Programming Phoenix LiveView*).

## When to invoke

| Skill | Use when |
|-------|----------|
| [elixir-core](elixir-core/SKILL.md) | Language idioms, pattern matching, `Enum`/`Stream`, protocols, `@spec` |
| [elixir-otp-design](elixir-otp-design/SKILL.md) | GenServer, Supervisor, Application, layered boundaries |
| [elixir-concurrency](elixir-concurrency/SKILL.md) | Task, Flow, GenStage, Broadway, back-pressure, pipelines |
| [elixir-phoenix](elixir-phoenix/SKILL.md) | Contexts, plugs, router, Ecto, channels (non-LiveView) |
| [elixir-liveview](elixir-liveview/SKILL.md) | LiveView lifecycle, components, streams, PubSub UI |
| [elixir-testing](elixir-testing/SKILL.md) | ExUnit, property tests, test structure |
| [elixir-review](elixir-review/SKILL.md) | PR review, idiomaticity check, pre-merge gate |

Skills cross-link at boundaries. OTP layering lives in `elixir-otp-design`; Phoenix contexts in `elixir-phoenix`; LiveView UI rules in `elixir-liveview`.
