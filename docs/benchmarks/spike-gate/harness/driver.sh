#!/usr/bin/env bash
# Interleaved gate rounds: for each round, wait for a quiet machine (<= 30 min), then run
# v0.6, spike A, spike B back to back (no further waiting inside the round).
# Usage: driver.sh FIRST LAST [--baseline-first]
set -uo pipefail
cd /data/Code/rust/id_effect
first=$1 last=$2
G=.tmp/spike-gate
if [[ ${3:-} == --baseline-first ]]; then
    MAXWAIT=1800 bash $G/round.sh v06 v0.6
fi
for r in $(seq "$first" "$last"); do
    MAXWAIT=1800 bash $G/round.sh v06 "v06-r$r"
    MAXWAIT=0 bash $G/round.sh a "a-r$r"
    MAXWAIT=0 bash $G/round.sh b "b-r$r"
done
echo DRIVER-DONE | tee -a docs/benchmarks/spike-gate/rounds.log
