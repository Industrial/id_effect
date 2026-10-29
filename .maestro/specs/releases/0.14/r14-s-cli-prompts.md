---
title: 'E13.1 Sweep — id_effect_cli interactive prompts and progress as effects over a Terminal capability: r14-s-cli-prompts'
slug: r14-s-cli-prompts
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'dx.cli#4 and dx.cli#5 (row dx.cli 75 → 100 once r14-p-toolchain promotes the row, D3; documented cap 89 until then) in crates/id_effect_cli (the CLI-building library; today lib.rs exposes only exit codes, `run_main` and `completions`, crates/id_effect_cli/src/lib.rs:50-59). A `Terminal` capability (trait: `is_interactive`, `read_line`, `write`, `width`) with `StdTerminal` (stdin, stderr, `std::io::IsTerminal`; reads run off the executor thread so a prompt never parks a worker) and `ScriptedTerminal` (queued inputs, captured output, interactive on or off) for tests. Feature `prompt`, no new dependency.'
  - 'dx.cli#4 interactive prompts as effects needing `Terminal`: `confirm(msg).default(bool)` → `Effect<bool, PromptError, _>`, `select(msg, items)` (numbered list, re-asks on an out-of-range answer) → index, `multi_select`, `input(msg).validate(f)` (re-asks with the validator''s message, at most `max_attempts`, default 3) → `String`, `password(msg)` (no echo on a TTY; never written to the captured output). Non-interactive (CI, a pipe): a prompt with a default answers it, one without fails `PromptError::NotInteractive` at once, never blocking. End of input fails `PromptError::Eof`; Ctrl-C interrupts the fiber, so `run_main` exits 130 (crates/id_effect_cli/src/run.rs, first Ctrl-C interrupts the root fiber).'
  - 'dx.cli#5 progress bars and spinners as effects (feature `progress`): `Progress::bar(total)` / `Progress::spinner(msg)` scoped (cleared on success, failure and interrupt, `acquire_release`), `inc(n)`, `set_message`, `with_progress(effect, bar)`; redraws are ticks on the executor clock (`sleep`), so a `TestExecutor` advances them deterministically; on a non-interactive terminal it writes a plain line at most every `quiet_interval` (default 5 s) and the final line, no carriage returns or ANSI.'
  - 'Tests: crates/id_effect_cli/tests/prompts.rs `confirm_reads_yes_no_and_default`, `select_reasks_on_out_of_range`, `input_validator_reasks_then_fails_after_max_attempts`, `password_is_not_echoed_to_output`, `non_interactive_uses_default_or_fails_fast`, `eof_fails_with_prompt_error`, `interrupt_during_prompt_exits_130`; crates/id_effect_cli/tests/progress.rs `bar_renders_on_virtual_clock_ticks`, `spinner_cleared_on_failure_and_interrupt`, `non_interactive_progress_writes_plain_lines`, `with_progress_counts_stream_items`.'
  - 'Book: new chapter crates/id_effect/book/src/part3/ch16-05-cli-prompts-progress.md (prompts, progress, testing both with `ScriptedTerminal`, CI behaviour), compiled by examples/book-snippets/src/part3.rs. Example crates/id_effect_cli/examples/020_prompts_progress.rs (confirm, select, a bar over a short job; answers its defaults when not on a TTY so `moon run-crate-examples` passes); examples/cli-minimal gains `--interactive` using `confirm` (and its test).'
  - 'Tier: integration (id_effect_cli, unchanged). Secure by default: no network surface; `password` input never reaches the captured output, logs or `Debug`; no new dependency.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_cli --all-features --summary-only --fail-under-regions 96`'
  - 'Gate: `cargo nextest run -p id_effect_cli --all-features`; `cargo nextest run -p cli_minimal`; `cargo run -p id_effect_cli --example 020_prompts_progress --features prompt,progress < /dev/null`; `cargo clippy -p id_effect_cli -p cli_minimal --all-targets --all-features -- -D warnings`; `bash scripts/check-no-run-blocking.sh id_effect_cli`; `cargo test -p book_snippets`'
  - 'Owns: `crates/id_effect_cli/src/{terminal,prompt,progress}.rs` (new), `crates/id_effect_cli/src/lib.rs` (module lines, re-exports and the crate-doc paragraph for prompts and progress; append-only), `crates/id_effect_cli/Cargo.toml` (features `prompt`, `progress` and the `[[example]] 020_prompts_progress` entry), `crates/id_effect_cli/tests/{prompts,progress}.rs`, `crates/id_effect_cli/examples/020_prompts_progress.rs`, `examples/cli-minimal/src/{lib,main}.rs` (the `--interactive` flag only), `examples/cli-minimal/tests/interactive.rs` (new), `examples/cli-minimal/Cargo.toml` (the id_effect_cli features), `crates/id_effect/book/src/part3/ch16-05-cli-prompts-progress.md`, append-only: `crates/id_effect/book/src/SUMMARY.md`, `examples/book-snippets/src/part3.rs` (include line), `examples/book-snippets/Cargo.toml` (`prompt`, `progress` features on the id_effect_cli line), `CHANGELOG.md`, `Cargo.lock`'
---

# r14-s-cli-prompts

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r14-t0-skeletons. Blocks r14-p-toolchain (wave 5 promotes `dx.cli`, which needs a
reference app or the tool to use the prompts in a CI test; the default is that `id-effect` itself does
not adopt them in this release). Scope-cut order 4 in the release spec.

Defaults the owner may overturn: (1) no `dialoguer` / `indicatif` / `crossterm`: the prompts are
line-based (a numbered `select`, not arrow keys) so they stay testable through `ScriptedTerminal` and
add no dependency; a raw-mode backend is a later, additive `Terminal` impl. (2) `password` no-echo on
a TTY needs termios; if that cannot be done without `unsafe` or a dependency (the crate forbids
`unsafe_code`), `password` warns that input is echoed on platforms where it cannot disable echo, and
the owner decides on `rpassword` (exact-pinned) at EXECUTE.
