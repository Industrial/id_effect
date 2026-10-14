#!/usr/bin/env bash
# Per-crate quality-bar conformance (docs/QUALITY_BAR.md, ADR 0010).
#
# Usage: bash scripts/tier-conformance.sh [package ...]
# Prints one row per package and exits 1 if any package FAILs its tier.
# Requires: cargo, jq, rg.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for tool in cargo jq rg; do
    command -v "$tool" >/dev/null || { echo "tier-conformance: missing $tool" >&2; exit 2; }
done

meta="$(cargo metadata --no-deps --format-version 1)"
if [[ -f labs/Cargo.toml ]]; then
    # Labs is a separate workspace (ADR 0010 Amendment 1); merge its packages in.
    labs_meta="$(cargo metadata --no-deps --format-version 1 --manifest-path labs/Cargo.toml)"
    meta="$(jq -s '{packages: (.[0].packages + .[1].packages)}' <<<"$meta"$'\n'"$labs_meta")"
fi
filter=("$@")

# Sum `rg -c` counts; rg exits 1 on no match, which must not trip pipefail.
count() { { rg -c --no-messages "$@" || true; } | awk -F: '{s+=$NF} END {print s+0}'; }

# Thresholds: minimum tests per non-blank src line, expressed as "1 per N".
density_for() {
    case "$1" in
        core|runtime) echo 40 ;;
        integration) echo 80 ;;
        *) echo 0 ;;
    esac
}

strict_tier() { [[ "$1" == core || "$1" == runtime || "$1" == integration ]]; }

fail_total=0
printf '%-26s %-12s %-6s %s\n' CRATE TIER RESULT DETAILS

while IFS=$'\t' read -r name manifest tier publish; do
    if ((${#filter[@]})) && [[ ! " ${filter[*]} " == *" $name "* ]]; then
        continue
    fi
    dir="$(dirname "$manifest")"
    rel="${dir#"$ROOT"/}"
    fails=()
    notes=()

    if [[ -z "$tier" || "$tier" == null ]]; then
        fails+=("T:no-tier")
        tier="-"
    fi

    root_rs="$dir/src/lib.rs"
    [[ -f "$root_rs" ]] || root_rs="$dir/src/main.rs"

    if strict_tier "$tier"; then
        # R: README with a rust example
        if [[ ! -f "$dir/README.md" ]]; then
            fails+=("R:no-readme")
        elif ! rg -q '^```rust' "$dir/README.md"; then
            fails+=("R:no-rust-example")
        fi
        # E: at least one example
        if ! compgen -G "$dir/examples/*.rs" >/dev/null && ! compgen -G "$dir/examples/*/main.rs" >/dev/null; then
            fails+=("E:no-examples")
        fi
        # D: deny(missing_docs)
        if [[ ! -f "$root_rs" ]] || ! rg -q '^#!\[(deny|forbid)\([^]]*missing_docs' "$root_rs"; then
            fails+=("D:missing_docs-not-denied")
        fi
        # S: stub markers outside tests
        stubs="$(count -i -e 'todo!\(' -e 'unimplemented!\(' -e '\bstub\b' -e '\bspike\b' \
      --glob '!**/tests/**' --glob '!**/*test*.rs' "$dir/src")"
        ((stubs > 0)) && fails+=("S:stub-markers=$stubs")
        # N: test density
        src_lines="$(count -v '^\s*$' "$dir/src")"
        tests="$(count -e '#\[(tokio::)?test\b' -e '#\[rstest\b' -e 'proptest!' --glob '!**/target/**' "$dir")"
        per="$(density_for "$tier")"
        need=$(( (src_lines + per - 1) / per ))
        if (( tests < need )); then
            fails+=("N:tests=$tests<need=$need(src=$src_lines)")
        else
            notes+=("tests=$tests/src=$src_lines")
        fi
    fi

    if strict_tier "$tier" || [[ "$tier" == maintenance ]]; then
        # A: crate-wide allow(dead_code|unused*) — rustc lints only; `clippy::unused_unit`
        # and friends are not dead-code suppression.
        if [[ -f "$root_rs" ]] && rg -q '^#!\[allow\(([^]]*[,[:space:]])?(dead_code|unused[a-z_]*)\b' "$root_rs"; then
            fails+=("A:crate-wide-allow")
        fi
    fi

    case "$tier" in
        labs)
            [[ "$rel" == labs/* ]] || fails+=("L:not-under-labs")
            [[ "$publish" == "[]" ]] || fails+=("L:publish-not-false")
            ;;
        merge) notes+=("pending-merge") ;;
        example) notes+=("example") ;;
    esac

    if ((${#fails[@]})); then
        result=FAIL
        fail_total=$((fail_total + 1))
        details="${fails[*]}"
    else
        result=PASS
        details="${notes[*]:-}"
    fi
    printf '%-26s %-12s %-6s %s\n' "$name" "$tier" "$result" "$details"
done < <(jq -r '.packages[] | [.name, .manifest_path, (.metadata.id_effect.tier // ""), (.publish | tojson)] | @tsv' <<<"$meta" | sort)

echo
if ((fail_total > 0)); then
    echo "tier-conformance: $fail_total package(s) FAIL"
    exit 1
fi
echo "tier-conformance: all packages PASS"
