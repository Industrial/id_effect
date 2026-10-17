#!/usr/bin/env bash
# Validate that workspace crates can be packaged for crates.io publish.
#
# Checks:
# 1) Publish-order: path+version pins must not require crates published later
#    (cross-level or later in the same publish.yml wave).
# 2) Dev-deps: workspace path deps in [dev-dependencies] must be path-only.
#    cargo publish still resolves version against crates.io for path+version.
# 3) cargo package with temporary [patch.crates-io] (sibling crates resolve locally).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

skip=id_effect_lint

# Publish levels (lower = earlier). This file is the single source of truth for publish
# order: .github/workflows/publish.yml runs `$0 --print-order` and publishes in that order.
# Labs crates (labs/, publish = false) are never listed (ADR 0010).
declare -A LEVEL=(
    [id_effect_macro]=0
    [id_effect_proc_macro]=0
    [id_effect]=1
    [id_effect_cli]=2
    [id_effect_fp]=2
    [id_effect_fsm]=3
    [id_effect_logger]=2
    [id_effect_optics]=2
    [id_effect_platform]=2
    [id_effect_process]=2
    [id_effect_resilience]=2
    [id_effect_sql]=2
    [id_effect_tokio]=2
    [id_effect_config]=3
    [id_effect_node]=4
    [id_effect_axum]=4
    [id_effect_opentelemetry]=4
    # Facade over level-4 crates (axum, opentelemetry, node), so the last level.
    [id_effect_kit]=5
    # The `id-effect` binary (ADR 0010 Amendment 4): publishes after the kit. It depends on no
    # workspace crate; the templates it embeds pin the kit at the workspace version.
    [id_effect_tool]=5
    # 0.9 crates (ADR 0010 Amendment 5), level 5 because they build on id_effect_axum (guards,
    # middleware, the in-process HTTP client) and id_effect_sql. authz first: id_effect_auth's
    # principal implements its `Principal`. All three publish before the kit, which may re-export them.
    [id_effect_authz]=5
    [id_effect_auth]=5
    # 0.9 stretch (E3.4): its `session` and `rate-limit` features build on id_effect_auth and
    # id_effect_axum, so it publishes after id_effect_auth.
    [id_effect_redis]=5
    [id_effect_test]=5
    # 0.10 crates (ADR 0010 Amendment 6). events, messaging and storage build on id_effect_sql and
    # id_effect_platform (level 2); jobs runs cron through id_effect_node (level 4); mail delivers
    # through jobs and console sits behind id_effect_authz guards, so both follow jobs.
    [id_effect_events]=3
    [id_effect_messaging]=3
    [id_effect_storage]=3
    [id_effect_jobs]=5
    [id_effect_mail]=5
    [id_effect_console]=5
)

# Publish order (levels ascending; within a level, dependencies first).
declare -a ORDER=(
    id_effect_macro id_effect_proc_macro
    id_effect
    id_effect_cli id_effect_fp id_effect_logger id_effect_optics id_effect_platform
    id_effect_process id_effect_resilience id_effect_sql id_effect_tokio
    id_effect_config id_effect_fsm id_effect_events id_effect_messaging id_effect_storage
    id_effect_node id_effect_axum id_effect_opentelemetry
    id_effect_authz id_effect_auth id_effect_redis id_effect_test
    id_effect_jobs id_effect_mail id_effect_console
    id_effect_kit id_effect_tool
)

# Crates in ORDER that are still labs crates awaiting promotion into crates/ (ADR 0010 rule 2).
# Remove the entry in the leaf that moves the crate (id_effect_events: r10-e7-4-promote).
declare -A PENDING_PROMOTION=(
    [id_effect_events]=1
)

if [[ "${1:-}" == "--print-order" ]]; then
    printf '%s\n' "${ORDER[@]}"
    exit 0
fi

# Every publishable workspace crate must have a level, and vice versa.
while IFS= read -r toml; do
    crate=$(grep -m1 '^name = ' "$toml" | sed 's/^name = "\(.*\)"/\1/')
    grep -q '^publish = false' "$toml" && continue
    if [[ -z "${LEVEL[$crate]:-}" ]]; then
        echo "ERROR: publishable crate $crate has no publish level in $0"
        exit 1
    fi
done < <(find crates -mindepth 2 -maxdepth 2 -name Cargo.toml | sort)
for crate in "${ORDER[@]}"; do
    pending="${PENDING_PROMOTION[$crate]:-}"
    if [[ -n "$pending" && ! -f "crates/$crate/Cargo.toml" && -f "labs/$crate/Cargo.toml" ]]; then
        continue
    fi
    if [[ ! -f "crates/$crate/Cargo.toml" ]]; then
        echo "ERROR: $crate is in the publish order but crates/$crate does not exist"
        exit 1
    fi
done
declare -A ORD_IDX=()
for i in "${!ORDER[@]}"; do ORD_IDX["${ORDER[$i]}"]=$i; done

failed=0

in_dev_deps_section() {
    local toml="$1" needle="$2"
    awk -v needle="$needle" '
    /^\[[^]]*\]/ { sec=$0 }
    index($0, needle) {
      print (sec ~ /^\[dev-dependencies\]/) ? "yes" : "no"
      exit
    }
  ' "$toml"
}

# --- Check 1+2: publish-order + path-only dev-deps ---
while IFS= read -r toml; do
    crate=$(grep -m1 '^name = ' "$toml" | sed 's/^name = "\(.*\)"/\1/')
    [[ "$crate" == "$skip" ]] && continue
    from_level=${LEVEL[$crate]:-}
    [[ -z "$from_level" ]] && continue
    from_ord=${ORD_IDX[$crate]:-}

    while IFS= read -r line; do
        dep=$(echo "$line" | sed -n 's/^\([a-zA-Z0-9_]*\) *=.*path *=.*version.*/\1/p')
        [[ -z "$dep" ]] && continue
        to_level=${LEVEL[$dep]:-}
        [[ -z "$to_level" ]] && continue

        if [[ "$(in_dev_deps_section "$toml" "$line")" == "yes" ]]; then
            echo "ERROR: $crate [dev-dependencies] pins $dep with path+version"
            echo "  → cargo publish resolves the version on crates.io even for path deps."
            echo "  → Use path-only: $dep = { path = \"../$dep\" }"
            echo "  in $toml: $line"
            failed=1
            continue
        fi

        if (( to_level > from_level )); then
            echo "ERROR: $crate (publish level $from_level) pins $dep version but $dep publishes at level $to_level"
            echo "  → cargo publish will look for $dep on crates.io before it exists."
            echo "  → For test-only deps use path without version; for runtime deps fix publish order."
            echo "  in $toml: $line"
            failed=1
            continue
        fi

        to_ord=${ORD_IDX[$dep]:-}
        if [[ -n "$from_ord" && -n "$to_ord" ]] && (( to_level == from_level && to_ord > from_ord )); then
            echo "ERROR: $crate pins $dep version but $dep publishes later in the same level ($from_level)"
            echo "  → Reorder publish.yml or use path-only if this is a test-only dependency."
            echo "  in $toml: $line"
            failed=1
        fi
    done < <(grep -E '^[a-zA-Z0-9_]+ = \{.*path = .*version =' "$toml" || true)
done < <(find crates -mindepth 2 -maxdepth 2 -name Cargo.toml | sort)

# --- Check 3: cargo package with local patch ---
cargo_dir=$(mktemp -d)
trap 'rm -rf "$cargo_dir"' EXIT
export CARGO_HOME="$cargo_dir"

mkdir -p "$CARGO_HOME"
{
    echo '[patch.crates-io]'
    while IFS= read -r toml; do
        crate=$(grep -m1 '^name = ' "$toml" | sed 's/^name = "\(.*\)"/\1/')
        [[ "$crate" == "$skip" ]] && continue
        dir=$(cd "$(dirname "$toml")" && pwd)
        echo "$crate = { path = \"$dir\" }"
    done < <(find crates -mindepth 2 -maxdepth 2 -name Cargo.toml | sort)
} > "$CARGO_HOME/config.toml"

while IFS= read -r toml; do
    crate=$(grep -m1 '^name = ' "$toml" | sed 's/^name = "\(.*\)"/\1/')
    [[ "$crate" == "$skip" ]] && continue

    if readme=$(grep -m1 '^readme = ' "$toml" | sed 's/^readme = "\(.*\)"/\1/'); then
        dir=$(dirname "$toml")
        if [[ ! -f "$dir/$readme" ]]; then
            echo "ERROR: $crate declares readme = \"$readme\" but $dir/$readme is missing"
            failed=1
            continue
        fi
    fi

    if ! cargo package --package "$crate" --no-verify --allow-dirty --quiet 2>/tmp/validate-pkg.err; then
        echo "ERROR: cargo package failed for $crate"
        sed 's/^/  /' /tmp/validate-pkg.err
        failed=1
    fi
done < <(find crates -mindepth 2 -maxdepth 2 -name Cargo.toml | sort)

if [[ "$failed" -ne 0 ]]; then
    exit 1
fi

echo "All publishable crates pass packaging validation"
