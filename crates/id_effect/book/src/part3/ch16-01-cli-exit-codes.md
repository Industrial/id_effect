# Exit codes for `main`

When a CLI finishes, the OS only sees an **8-bit exit status**. **id_effect** distinguishes richer outcomes ([`Exit`](https://docs.rs/id_effect/latest/id_effect/enum.Exit.html), [`Cause`](https://docs.rs/id_effect/latest/id_effect/enum.Cause.html)); at the process edge you collapse that into [`std::process::ExitCode`](https://doc.rust-lang.org/std/process/struct.ExitCode.html).

## The `Exit` / `Cause` mapping

Running an effect at the edge (`block_on`, or `id_effect_cli::run_main`) gives you its full `Exit`. Leaf causes map as follows; composite [`Cause::Both`](https://docs.rs/id_effect/latest/id_effect/enum.Cause.html#variant.Both) / [`Cause::Then`](https://docs.rs/id_effect/latest/id_effect/enum.Cause.html#variant.Then) use the **maximum** byte so “stronger” failures dominate:

| Pattern | Byte | Meaning |
|---------|------|---------|
| `Exit::Success` | `0` | OK |
| `Cause::Fail(_)` | `1` | expected typed failure |
| `Cause::Die(_)` | `101` | defect (a panic or `die`) |
| `Cause::Interrupt(_)` | `130` | interruption (the convention many shells use for **SIGINT**) |
| `Cause::Both` / `Cause::Then` | `max(left, right)` | recurse |

Helpers: [`exit_code_for_exit`](https://docs.rs/id_effect_cli/latest/id_effect_cli/fn.exit_code_for_exit.html), [`exit_code_for_cause`](https://docs.rs/id_effect_cli/latest/id_effect_cli/fn.exit_code_for_cause.html), [`cause_max_exit_byte`](https://docs.rs/id_effect_cli/latest/id_effect_cli/fn.cause_max_exit_byte.html). For a plain `Result` (`Ok` → `0`, `Err` → `1`) there is [`exit_code_for_result`](https://docs.rs/id_effect_cli/latest/id_effect_cli/fn.exit_code_for_result.html).

## What `run_main` prints

[`run_main`](https://docs.rs/id_effect_cli/latest/id_effect_cli/fn.run_main.html) maps the whole `Exit` with the table above and reports a failure on **stderr**:

| Outcome | stderr | exit code |
|---|---|---|
| success | – | `0` |
| typed failure `Fail(e)` | `error: {e:?}` | `1` |
| defect `Die` | `error: defect: …` | `101` |
| interrupt | `error: interrupted (…)` | `130` |

Typed failures are printed with `Debug`; for user-facing output, map your error to a `String` (or a thin `Display` wrapper) first: `run_main(program.map_err(|e| e.to_string()), env, RunMainConfig::minimal())`.

## Practical guidance

- Use **`1`** for “the command could not complete successfully” (missing flag, validation error, upstream HTTP 4xx mapped to your `E`, …).
- Reserve **`101`** for “the program detected an internal defect” — a `Cause::Die` (a `die(..)` or a panic inside an effect, which the runtime catches).
- **`130`** appears when the root fiber is interrupted.

If you need richer machine output, prefer **stderr JSON** or a dedicated output file — do not overload exit codes beyond what your operators can act on.
