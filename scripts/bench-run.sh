#!/usr/bin/env bash
# Run the D9 gate benches and save them as a named criterion baseline.
#
# Usage: bash scripts/bench-run.sh [--src DIR | --ref REF] [--baseline NAME] [--target-dir DIR]
#                                  [--only PKG:BENCH]... [--skip PKG:BENCH]... [-- CRITERION_ARGS...]
#   --src DIR         workspace to bench (default: this checkout)
#   --ref REF         `git archive REF` into .tmp/bench-<NAME> and bench that (no worktree);
#                     NAME is REF without a leading bench/, so --ref bench/v0.6 benches
#                     .tmp/bench-v0.6 as baseline v0.6
#   --baseline NAME   criterion baseline name (default: head, or the NAME derived from REF)
#   --target-dir DIR  shared CARGO_TARGET_DIR (default: ./target), so every baseline lands in
#                     one target/criterion for scripts/bench-export.sh
#   --only PKG:BENCH  run only this bench (repeatable; its features are the list's)
#   --skip PKG:BENCH  leave this bench out (repeatable; CI runs h3_loss on its own, under netem)
# Records the benched SHA in <target>/criterion/.bench-run/<NAME>.sha for bench-export.sh.
# Requires: cargo, git, tar.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# package:bench[:features] entries; every gates.toml id lives in one of these. A bench the
# benched tree does not have (an older --ref) is skipped with a warning.
GATE_BENCHES=(
    id_effect:phase0_baseline
    id_effect:gate_core
    id_effect_tokio:gate_fiber
    id_effect_axum:gate_request
    id_effect_rpc:gate_rpc:postcard
    id_effect_rpc:h3_loss:http3
    id_effect_sql:repository_eager:repository,sqlite
    id_effect_i18n:format
    id_effect_axum:ui_render:ui
)

usage() { sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2; exit 2; }

src="" ref="" baseline="" target="${CARGO_TARGET_DIR:-$ROOT/target}"
only=() skip=()
while (($#)); do
    case "$1" in
        --src) src="${2:?--src needs a directory}"; shift 2 ;;
        --ref) ref="${2:?--ref needs a git ref}"; shift 2 ;;
        --baseline) baseline="${2:?--baseline needs a name}"; shift 2 ;;
        --target-dir) target="${2:?--target-dir needs a directory}"; shift 2 ;;
        --only) only+=("${2:?--only needs PKG:BENCH}"); shift 2 ;;
        --skip) skip+=("${2:?--skip needs PKG:BENCH}"); shift 2 ;;
        --) shift; break ;;
        -h | --help) usage ;;
        *) echo "bench-run: unknown argument: $1" >&2; usage ;;
    esac
done
[[ -n "$src" && -n "$ref" ]] && { echo "bench-run: --src and --ref are exclusive" >&2; exit 2; }

for tool in cargo git tar; do
    command -v "$tool" >/dev/null || { echo "bench-run: missing $tool" >&2; exit 2; }
done

if [[ -n "$ref" ]]; then
    sha="$(git rev-parse --verify "$ref^{commit}")"
    name="${ref#bench/}"
    name="${name//\//-}"
    src="$ROOT/.tmp/bench-$name"
    rm -rf "$src"
    mkdir -p "$src"
    git archive "$sha" | tar -x -C "$src"
    echo "$sha" >"$src/.bench-sha"
    baseline="${baseline:-$name}"
    echo "bench-run: exported $ref ($sha) to ${src#"$ROOT"/}"
fi
src="$(cd "${src:-$ROOT}" && pwd)"
baseline="${baseline:-head}"
if [[ -f "$src/.bench-sha" ]]; then
    sha="$(<"$src/.bench-sha")"
else
    sha="$(git -C "$src" rev-parse HEAD 2>/dev/null || echo unknown)"
fi
[[ -f "$src/Cargo.toml" ]] || { echo "bench-run: no Cargo.toml in $src" >&2; exit 2; }

governor="$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)"
[[ "$governor" == performance ]] || echo "bench-run: warning: CPU governor is '$governor', not 'performance'" >&2

mkdir -p "$target"
target="$(cd "$target" && pwd)"
export CARGO_TARGET_DIR="$target"

# The selected entries, with their features from GATE_BENCHES.
selected=()
for entry in "${GATE_BENCHES[@]}"; do
    key="${entry%:*}"
    [[ "$entry" == *:*:* ]] || key="$entry"
    if ((${#only[@]})); then
        keep=0
        for o in "${only[@]}"; do [[ "$o" == "$key" ]] && keep=1; done
        ((keep)) || continue
    fi
    for s in "${skip[@]}"; do [[ "$s" == "$key" ]] && continue 2; done
    selected+=("$entry")
done
for o in "${only[@]}"; do
    known=0
    for entry in "${GATE_BENCHES[@]}"; do [[ "$entry" == "$o" || "$entry" == "$o":* ]] && known=1; done
    ((known)) || selected+=("$o")
done

echo "bench-run: baseline '$baseline' from ${src} @ ${sha:0:12} into $target/criterion"
for entry in "${selected[@]}"; do
    IFS=: read -r pkg bench features <<<"$entry"
    if [[ ! -f "$src/crates/$pkg/benches/$bench.rs" ]]; then
        echo "bench-run: warning: $pkg has no bench $bench in $src, skipped" >&2
        continue
    fi
    echo "bench-run: $pkg --bench $bench${features:+ --features $features}"
    cargo bench --manifest-path "$src/Cargo.toml" -p "$pkg" ${features:+--features "$features"} \
        --bench "$bench" -- --save-baseline "$baseline" --noplot "$@"
done

mkdir -p "$target/criterion/.bench-run"
echo "$sha" >"$target/criterion/.bench-run/$baseline.sha"
echo "bench-run: done; export with: bash scripts/bench-export.sh $baseline OUT.json"
