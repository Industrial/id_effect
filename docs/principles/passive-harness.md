# passive-harness

## Rule

Maestro never schedules background work. No `set` + `Interval`, no detached daemons, no LLM calls, no auto-launching subprocesses.

## Rationale

Passive harness is the load-bearing invariant: a single repo-tracked state directory can be safely shared across agents and operators only when nothing is mutating the world while it is being read.

## Scan Command

! rg -n "setInterval|setTimeout|child_process\.fork|spawn.*detached|new Worker\(" --glob 'src/{config,providers,repo,runtime,service,types,ui}/**' --glob '!**/*.test.ts'

## Fix Recipe

1. Remove the background scheduler.
2. Turn the work into a CLI verb the agent or external cron invokes.
3. Record the resulting state change as an evidence row.
