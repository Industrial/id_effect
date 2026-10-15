#!/usr/bin/env bash
# Usage: round.sh WHAT BASELINE   (WHAT = v06 | a | b | probe)
# Waits for a quiet machine (no rustc / cargo build processes; poll 60 s, max 30 min), then runs
# the bench set, sampling rustc count every 5 s. Appends one line to docs/benchmarks/spike-gate/rounds.log.
set -uo pipefail
cd /data/Code/rust/id_effect
what=$1 baseline=$2
OUT=docs/benchmarks/spike-gate
LOGDIR=$OUT/raw
mkdir -p "$LOGDIR"
busy() {
    local r c m n
    r=$(pgrep -c -x rustc || true)
    c=$(pgrep -c -x cargo || true)
    m=$(pgrep -c -x moon || true)
    n=$(pgrep -c -x cargo-nextest || true)
    echo "$((r + c + m + n))"
}
# Quiet at start = no rustc/cargo/moon/nextest and 1-min load average below 8 (of 32 CPUs; the
# desktop's own background load floor is about 5).
loadhi() { awk '{ exit !($1 >= 8.0) }' /proc/loadavg; }
waited=0
pre="$(busy)/load=$(cut -d' ' -f1 /proc/loadavg)"
maxwait=${MAXWAIT:-1800}
while { [[ $(busy) -gt 0 ]] || loadhi; } && [[ $waited -lt $maxwait ]]; do
    sleep 60
    waited=$((waited + 60))
done
start_busy="$(busy)/load=$(cut -d' ' -f1 /proc/loadavg)"
start=$(date -u +%FT%TZ)
samples="$LOGDIR/$baseline.busy"
: >"$samples"
( while true; do echo "$(busy)" >>"$samples"; sleep 5; done ) &
sampler=$!
case "$what" in
    v06)
        bash scripts/bench-run.sh --src .tmp/bench-v0.6 --baseline "$baseline" --target-dir .tmp/target-bench-v06 \
            >"$LOGDIR/$baseline.log" 2>&1
        ;;
    a | b)
        pkg=kernel_$what
        bench=gate; [[ $what == b ]] && bench=gate_b
        CARGO_TARGET_DIR=$PWD/.tmp/target-spikes cargo bench --manifest-path spikes/Cargo.toml -p "$pkg" --bench "$bench" \
            -- --save-baseline "$baseline" --noplot >"$LOGDIR/$baseline.log" 2>&1
        ;;
    probe)
        cargo bench --manifest-path .tmp/spike-gate/probe/Cargo.toml --target-dir .tmp/target-probe --bench probe \
            -- --save-baseline "$baseline" --noplot >"$LOGDIR/$baseline.log" 2>&1
        ;;
esac
rc=$?
kill "$sampler" 2>/dev/null
wait "$sampler" 2>/dev/null
end=$(date -u +%FT%TZ)
# The sampler sees our own `cargo bench` (1) while it runs; subtract it.
maxb=$(sort -n "$samples" | tail -1)
own=1
contended=quiet
[[ ( $maxwait -gt 0 && $waited -ge $maxwait ) || $start_busy != 0/* || $((maxb - own)) -gt 0 ]] && contended=contended
echo "$baseline what=$what rc=$rc start=$start end=$end busy_before=$pre waited_s=$waited busy_at_start=$start_busy max_busy_during=$maxb (own cargo=$own) => $contended" | tee -a "$OUT/rounds.log"
