---
title: 0.8 E1.10 — CLI app kit: graceful SIGINT and shell completions
slug: r08-e1-10-cli-kit
mode: light
work_type: feature
risk_class: medium
version: 1
acceptance_criteria:
  - 'run_main (crates/id_effect_cli/src/run.rs:60) runs the program on an explicit root FiberContext and installs a tokio ctrl_c watcher that calls request_interrupt (second Ctrl-C force-exits 130); opt-out via RunMainConfig (made #[non_exhaustive] with a builder); id_effect_cli enables tokio feature signal.'
  - 'Test (cfg(unix)): ie_cli_probe --mode sleep (infinite sleep inside acquire_release whose finalizer prints released) receives SIGINT and exits 130 with released on stdout and error: interrupted on stderr; existing cli_probe_exit cases (0, 1, 101) and run.rs unit tests pass.'
  - 'Optional feature completions = [clap, dep:clap_complete]: id_effect_cli::completions::{generate, CompletionsArgs}; examples/cli-minimal gains `completions <shell>`; a test checks bash output contains complete -F and zsh/fish exit 0.'
  - 'cargo clippy -p id_effect_cli --all-features -D warnings clean; builds with --no-default-features; MIGRATION-0.8 entry for RunMainConfig; scorecard evidence for dx.cli #3 and #7.'
non_goals:
  - Interactive prompts and progress bars (dx.cli #4, #5).
---

# 0.8 E1.10 — CLI app kit

Moves dx.cli 4/8 -> 6/8. Release spec: `.maestro/specs/releases/0.8.0.md`.
