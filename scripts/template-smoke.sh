#!/usr/bin/env bash
# Generate `id-effect new` templates outside the repository and check them against this checkout.
#
# Each variant is generated with `--local-workspace <repo>` into a temporary directory, gets the
# root Cargo.lock when there is one (same dependency versions as the workspace), and must pass:
#   cargo fmt --check · cargo build --all-targets (no unused [patch] entry) · cargo clippy
#   --all-targets -D warnings · cargo test · no id_effect crate resolved from crates.io ·
#   actionlint on its .github/workflows/ci.yml (when actionlint is installed).
# `service` covers two variants: the default (`--db none --no-otel`, whose tests boot the binary
# with nothing else running) and `--db pg --otel` (built, linted and tested without a database).
#
# Usage:
#   scripts/template-smoke.sh [TEMPLATE...] [--docker] [--keep]
#     TEMPLATE  service | worker | cli | cluster (default: all four)
#     --docker  also `docker build` the generated service and probe /health in a container (the
#               checkout's crates are copied into the build context and patched in relatively)
#     --keep    keep the temporary directory
#
# Environment:
#   SMOKE_DIR         where the projects go (default: a new `mktemp -d`, removed afterwards
#                     unless --keep; must be outside the repo)
#   SMOKE_TARGET_DIR  the target dir the projects share (default: $SMOKE_DIR/target), so later
#                     variants reuse the dependencies the first one compiled
# The `id-effect` binary is built with the caller's CARGO_TARGET_DIR (default: <repo>/target).
#
# Output: `template-smoke: <variant> ok (build <s>s, clippy <s>s, test <s>s)` per variant and
# `template-smoke: docker ok (build <s>s)`. Exit status: 0 ok, 1 a check failed, 2 misuse.
# Requires: bash >= 4, cargo (with rustfmt and clippy), git, GNU date; --docker: docker, curl.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
ALL=(service worker cli cluster)

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed -E 's/^# ?//'
}

die() {
    echo "template-smoke: $*" >&2
    exit 2
}

fail() {
    echo "template-smoke: FAIL: $*" >&2
    exit 1
}

now_ns() {
    date +%s%N
}

seconds_since() {
    awk -v ns=$(($(now_ns) - $1)) 'BEGIN { printf "%.1f", ns / 1e9 }'
}

templates=()
docker=0
keep=0
for arg in "$@"; do
    case "${arg}" in
        service | worker | cli | cluster) templates+=("${arg}") ;;
        --docker) docker=1 ;;
        --keep) keep=1 ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "unknown argument: ${arg} (see --help)" ;;
    esac
done
((${#templates[@]})) || templates=("${ALL[@]}")

if [[ -z "${SMOKE_DIR:-}" ]]; then
    SMOKE_DIR="$(mktemp -d)"
    ((keep)) || trap 'rm -rf "${SMOKE_DIR}"' EXIT
fi
mkdir -p "${SMOKE_DIR}"
SMOKE_DIR="$(cd "${SMOKE_DIR}" && pwd -P)"
case "${SMOKE_DIR}/" in
    "${ROOT}/"*) die "SMOKE_DIR ${SMOKE_DIR} is inside the repository; generated projects must live outside it" ;;
esac
export CARGO_TARGET_DIR_SMOKE="${SMOKE_TARGET_DIR:-${SMOKE_DIR}/target}"
echo "template-smoke: projects in ${SMOKE_DIR}" >&2

# The generator itself, built from this checkout.
cd "${ROOT}"
cargo build --quiet -p id_effect_tool --bin id-effect
tool_target="${CARGO_TARGET_DIR:-target}"
[[ "${tool_target}" == /* ]] || tool_target="${ROOT}/${tool_target}"
BIN="${tool_target}/debug/id-effect"
[[ -x "${BIN}" ]] || fail "no id-effect binary at ${BIN}"

# Generates the variant LABEL (`id-effect new smoke-LABEL ARGS...`) and runs every check on it.
check_variant() {
    local label="$1" dest="${SMOKE_DIR}/$1" log t build_s clippy_s test_s
    shift
    rm -rf "${dest}"
    "${BIN}" new "smoke-${label}" --dest "${dest}" --local-workspace "${ROOT}" "$@" >/dev/null ||
    fail "${label}: id-effect new failed"
    [[ ! -f "${ROOT}/Cargo.lock" ]] || cp "${ROOT}/Cargo.lock" "${dest}/Cargo.lock"
    log="${dest}/build.log"
    (
        cd "${dest}"
        export CARGO_TARGET_DIR="${CARGO_TARGET_DIR_SMOKE}"
        cargo fmt --all --check || fail "${label}: cargo fmt --check"
        t="$(now_ns)"
        cargo build --all-targets 2>&1 | tee "${log}" >&2 || fail "${label}: cargo build"
        build_s="$(seconds_since "${t}")"
        if grep -q 'was not used in the crate graph' "${log}"; then
            fail "${label}: a [patch.crates-io] entry is unused (Options::workspace_crates is stale)"
        fi
        t="$(now_ns)"
        cargo clippy --all-targets --quiet -- -D warnings || fail "${label}: cargo clippy"
        clippy_s="$(seconds_since "${t}")"
        t="$(now_ns)"
        cargo test --quiet || fail "${label}: cargo test"
        test_s="$(seconds_since "${t}")"
        if awk '/^name = "id_effect/ { dep = 1; next } dep && /^source = "registry/ { found = 1 } /^$/ { dep = 0 } END { exit !found }' Cargo.lock; then
            fail "${label}: an id_effect crate resolved from crates.io instead of the checkout"
        fi
        if command -v actionlint >/dev/null; then
            actionlint .github/workflows/ci.yml || fail "${label}: actionlint"
        fi
        echo "template-smoke: ${label} ok (build ${build_s}s, clippy ${clippy_s}s, test ${test_s}s)"
    )
}

# Builds the default service as a Docker image and probes /health in a container. The image has
# no access to the checkout, so the crates are copied into the build context as _ws/ and the
# [patch.crates-io] paths rewritten relative to it.
check_docker() {
    local ws="${SMOKE_DIR}/docker-ws" ctx="${SMOKE_DIR}/docker-service" image="smoke-docker:local"
    local t build_s cid port answer="" i
    command -v docker >/dev/null || fail "docker: not installed"
    rm -rf "${ws}" "${ctx}"
    mkdir -p "${ws}"
    cp "${ROOT}/Cargo.toml" "${ws}/Cargo.toml"
    # Tracked and untracked (not ignored) files of the working tree; deleted ones are skipped.
    git -C "${ROOT}" ls-files -co --exclude-standard -- crates ':!crates/*/book/**' ':!crates/id_effect_tool/**' |
    while IFS= read -r file; do
        if [[ -f "${ROOT}/${file}" ]]; then printf '%s\n' "${file}"; fi
    done |
    tar -C "${ROOT}" -cf - -T - | tar -C "${ws}" -xf -
    ws="$(cd "${ws}" && pwd -P)"
    "${BIN}" new smoke-docker --template service --dest "${ctx}" --local-workspace "${ws}" >/dev/null ||
    fail "docker: id-effect new failed"
    mv "${ws}" "${ctx}/_ws"
    sed -i "s#\"${ws}/crates/#\"_ws/crates/#" "${ctx}/Cargo.toml"
    [[ ! -f "${ROOT}/Cargo.lock" ]] || cp "${ROOT}/Cargo.lock" "${ctx}/Cargo.lock"
    t="$(now_ns)"
    if ! docker build -t "${image}" "${ctx}" >"${SMOKE_DIR}/docker-build.log" 2>&1; then
        tail -n 40 "${SMOKE_DIR}/docker-build.log" >&2
        fail "docker: build"
    fi
    build_s="$(seconds_since "${t}")"
    cid="$(docker run -d -P "${image}")"
    port="$(docker port "${cid}" 8080/tcp | head -n1 | sed 's/.*://')"
    for i in $(seq 1 60); do
        answer="$(curl -fsS "http://127.0.0.1:${port}/health" 2>/dev/null || true)"
        [[ -z "${answer}" ]] || break
        sleep 0.5
    done
    docker rm -f "${cid}" >/dev/null
    [[ -n "${answer}" ]] || fail "docker: no answer on /health after ${i} tries"
    echo "template-smoke: docker ok (build ${build_s}s, /health answered: ${answer})"
}

for template in "${templates[@]}"; do
    check_variant "${template}" --template "${template}"
    if [[ "${template}" == service ]]; then
        check_variant service-pg-otel --template service --db pg --otel
    fi
done
if ((docker)); then
    check_docker
fi
