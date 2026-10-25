#!/usr/bin/env bash
# Generate `id-effect new` templates outside the repository and check them against this checkout.
#
# Each variant is generated with `--local-workspace <repo>` into a temporary directory, gets the
# root Cargo.lock when there is one (same dependency versions as the workspace), and must pass:
#   cargo fmt --check · cargo build --all-targets (no unused [patch] entry) · cargo clippy
#   --all-targets -D warnings · cargo test · no id_effect crate resolved from crates.io ·
#   actionlint on its .github/workflows/ci.yml (when actionlint is installed).
# `service` covers four variants: the default (`--db none --no-otel`, whose tests boot the binary
# with nothing else running), `--db pg --otel` (built, linted and tested without a database),
# `--jobs --console` (booted like the default) and `--db pg --otel --jobs --console`.
# The default `service` also gets its TypeScript client (`id-effect rpc gen ts`, checked with
# `deno check`) and a round trip against the running binary: the binary is started with the
# `id-effect dev --cert-only` development certificate, `Alt-Svc` must name its port, a Deno
# script calls `Greeter.greet` through the generated client over TCP (TLS), and curl calls it
# over HTTP/3 on the same port (`--http3-only`). Without `deno`, or a curl without HTTP/3, that
# part is skipped with a message (SMOKE_REQUIRE_ROUND_TRIP=1 fails instead).
# Before the checks, `id-effect generate job send_report` adds a job to `service` and
# `service-jobs-console` (into the src/jobs/ that `--jobs` generated) and (with
# `--queue reports --cron`) to `worker`; its test must pass.
# A fifth `service` variant, `service-admin`, gets a tenant-scoped `Document` entity written
# against the kit's re-exports (`id_effect_kit::sql`) and `id-effect generate admin Document`:
# the generated admin imports `id_effect_kit::admin`, the kit gains feature `admin` (no direct
# id_effect_admin dependency), and the admin's tests must pass with the rest.
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
# Requires: bash >= 4, cargo (with rustfmt and clippy), git, GNU date, curl; for the round trip
# deno and a curl with HTTP/3; --docker: docker.
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

# Adds a job with `id-effect generate job` to the variants that get one; prints its test's name.
generate_job() {
    local label="$1" dest="$2"
    case "${label}" in
        service | service-jobs-console) "${BIN}" generate job send_report --path "${dest}" ;;
        worker) "${BIN}" generate job send_report --queue reports --cron "0 6 * * *" --path "${dest}" ;;
        *) return 0 ;;
    esac >/dev/null || fail "${label}: id-effect generate job failed"
    echo "jobs::send_report::tests::a_queued_job_runs_once_and_succeeds"
}

# Adds a `Document` entity (through the kit's re-exports) and its admin with `id-effect generate
# admin` to the variants that get one; prints one of the admin's tests.
generate_admin() {
    local label="$1" dest="$2" deps
    [[ "${label}" == service-admin ]] || return 0
    cat >"${dest}/src/documents.rs" <<'RS'
//! The service's documents.

use id_effect_kit::prelude::{Entity, FromRow};

/// A document of an organization.
#[derive(Clone, Debug, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(crate = id_effect_kit::sql, table = "documents", tenant = "org_id")]
pub struct Document {
    #[entity(id, generated)]
    pub id: i64,
    pub org_id: i64,
    #[entity(editable)]
    pub title: String,
    #[entity(editable)]
    pub status: String,
    pub reviewed: bool,
    #[entity(version)]
    pub version: i64,
}
RS
    sed -i 's/^pub mod rpc;$/pub mod documents;\npub mod rpc;/' "${dest}/src/lib.rs"
    grep -q '^pub mod documents;$' "${dest}/src/lib.rs" ||
    fail "${label}: no 'pub mod rpc;' in src/lib.rs to declare the entity module next to"
    "${BIN}" generate admin Document --path "${dest}" >/dev/null ||
    fail "${label}: id-effect generate admin failed"
    grep -q '^use id_effect_kit::admin::' "${dest}/src/admin/document.rs" ||
    fail "${label}: the generated admin does not import id_effect_kit::admin"
    deps="$(sed -n '/^\[dependencies\]/,/^\[/p' "${dest}/Cargo.toml")"
    grep -q '^id_effect_kit = .*"admin"' <<<"${deps}" ||
    fail "${label}: generate admin did not turn on the kit's admin feature"
    if grep -q '^id_effect_admin' <<<"${deps}"; then
        fail "${label}: a direct id_effect_admin dependency (the kit's admin feature brings it)"
    fi
    echo "admin::document::tests::every_page_is_behind_the_guard"
}

# Generates the variant LABEL (`id-effect new smoke-LABEL ARGS...`) and runs every check on it.
check_variant() {
    local label="$1" dest="${SMOKE_DIR}/$1" log t build_s clippy_s test_s job_test admin_test listed
    shift
    rm -rf "${dest}"
    "${BIN}" new "smoke-${label}" --dest "${dest}" --local-workspace "${ROOT}" "$@" >/dev/null ||
    fail "${label}: id-effect new failed"
    job_test="$(generate_job "${label}" "${dest}")"
    admin_test="$(generate_admin "${label}" "${dest}")"
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
        if [[ -n "${job_test}" ]]; then
            listed="$(cargo test --quiet --lib -- --list 2>/dev/null)" || fail "${label}: cargo test --list"
            [[ "${listed}" == *"${job_test}: test"* ]] ||
            fail "${label}: the generated job's test ${job_test} is not among the lib tests"
        fi
        if [[ -n "${admin_test}" ]]; then
            listed="$(cargo test --quiet --lib -- --list 2>/dev/null)" || fail "${label}: cargo test --list"
            [[ "${listed}" == *"${admin_test}: test"* ]] ||
            fail "${label}: the generated admin's test ${admin_test} is not among the lib tests"
        fi
        if awk '/^name = "id_effect/ { dep = 1; next } dep && /^source = "registry/ { found = 1 } /^$/ { dep = 0 } END { exit !found }' Cargo.lock; then
            fail "${label}: an id_effect crate resolved from crates.io instead of the checkout"
        fi
        if command -v actionlint >/dev/null; then
            actionlint .github/workflows/ci.yml || fail "${label}: actionlint"
        fi
        echo "template-smoke: ${label} ok (build ${build_s}s, clippy ${clippy_s}s, test ${test_s}s)"
    )
}

# Prints a free TCP port on 127.0.0.1 (UDP on the same number is checked by the server's bind).
free_port() {
    local port
    for _ in $(seq 1 50); do
        port=$((20000 + RANDOM % 40000))
        if ! (exec 3<>"/dev/tcp/127.0.0.1/${port}") 2>/dev/null; then
            echo "${port}"
            return 0
        fi
    done
    fail "no free port found"
}

# Skips (or, with SMOKE_REQUIRE_ROUND_TRIP=1, fails) the round trip for want of a tool.
round_trip_unavailable() {
    if [[ "${SMOKE_REQUIRE_ROUND_TRIP:-0}" == 1 ]]; then
        fail "round trip: $*"
    fi
    echo "template-smoke: round trip skipped: $*" >&2
}

# The generated service's TypeScript client and a round trip over TCP and HTTP/3 against its
# binary, started with the development certificate.
check_round_trip() {
    local label="$1" dest="${SMOKE_DIR}/$1" exe certs port base pid="" answer="" headers h3 i
    local TLS_CERT_FILE="" TLS_KEY_FILE="" ID_EFFECT_DEV_CA_FILE=""
    exe="${CARGO_TARGET_DIR_SMOKE}/debug/smoke-${label}"
    [[ -x "${exe}" ]] || fail "${label}: no binary at ${exe}"
    "${BIN}" rpc gen ts --contract "${dest}/rpc.contract.json" --out "${dest}/web/src/rpc" ||
    fail "${label}: id-effect rpc gen ts"
    [[ -f "${dest}/web/src/rpc/client.ts" ]] || fail "${label}: no generated client.ts"
    if ! command -v deno >/dev/null; then
        round_trip_unavailable "deno is not on PATH"
        return 0
    fi
    cat >"${dest}/web/round_trip.ts" <<'TS'
// template-smoke: `deno run round_trip.ts <rpc url>` calls Greeter.greet through the generated client.
import { GreeterV1Client } from "./src/rpc/client.ts";

const greeter = new GreeterV1Client({ url: Deno.args[0] });
const answer = await greeter.greet({ name: "deno" });
if (answer._tag !== "Success" || answer.value.message !== "hello, deno") {
    console.error(JSON.stringify(answer));
    Deno.exit(1);
}
const refused = await greeter.greet({ name: "" });
if (refused._tag !== "Failure" || JSON.stringify(refused).indexOf("InvalidName") < 0) {
    console.error(JSON.stringify(refused));
    Deno.exit(1);
}
console.log(answer.value.message);
TS
    export DENO_DIR="${SMOKE_DIR}/deno" DENO_NO_UPDATE_CHECK=1 NO_COLOR=1
    deno check --no-config "${dest}/web/round_trip.ts" >&2 || fail "${label}: deno check"

    certs="$("${BIN}" dev --cert-only --cert-dir "${SMOKE_DIR}/dev-cert")" ||
    fail "${label}: id-effect dev --cert-only"
    TLS_CERT_FILE="$(sed -n 's/^TLS_CERT_FILE=//p' <<<"${certs}")"
    TLS_KEY_FILE="$(sed -n 's/^TLS_KEY_FILE=//p' <<<"${certs}")"
    ID_EFFECT_DEV_CA_FILE="$(sed -n 's/^ID_EFFECT_DEV_CA_FILE=//p' <<<"${certs}")"
    port="$(free_port)"
    base="https://127.0.0.1:${port}"
    (
        cd "${dest}"
        exec env -u ID_EFFECT_PROFILE HOST=127.0.0.1 PORT="${port}" \
            TLS_CERT_FILE="${TLS_CERT_FILE}" TLS_KEY_FILE="${TLS_KEY_FILE}" "${exe}"
    ) >"${dest}/serve.log" 2>&1 &
    pid=$!
    for i in $(seq 1 100); do
        answer="$(curl -fsS --cacert "${ID_EFFECT_DEV_CA_FILE}" "${base}/health" 2>/dev/null || true)"
        [[ -z "${answer}" ]] || break
        sleep 0.1
    done
    if [[ -z "${answer}" ]]; then
        kill "${pid}" 2>/dev/null || true
        cat "${dest}/serve.log" >&2
        fail "${label}: no answer on ${base}/health after ${i} tries"
    fi
    headers="$(curl -fsS -o /dev/null -D - --cacert "${ID_EFFECT_DEV_CA_FILE}" "${base}/health")"
    if ! grep -qi "^alt-svc: h3=\":${port}\"" <<<"${headers}"; then
        kill "${pid}" 2>/dev/null || true
        fail "${label}: no Alt-Svc h3 on port ${port} in: ${headers}"
    fi
    if ! deno run --no-config --no-remote --no-prompt --allow-net="127.0.0.1:${port}" \
        --cert "${ID_EFFECT_DEV_CA_FILE}" "${dest}/web/round_trip.ts" "${base}/rpc" >&2; then
        kill "${pid}" 2>/dev/null || true
        fail "${label}: the Deno round trip over TCP failed"
    fi
    if curl -V | grep -q 'HTTP3'; then
        h3="$(curl -fsS --http3-only --cacert "${ID_EFFECT_DEV_CA_FILE}" \
            -H 'content-type: application/json' -H 'x-rpc: 1' -d '{"name":"h3"}' \
            "${base}/rpc/Greeter.greet" 2>&1 || true)"
        if [[ "${h3}" != *'hello, h3'* ]]; then
            kill "${pid}" 2>/dev/null || true
            cat "${dest}/serve.log" >&2
            fail "${label}: the HTTP/3 round trip failed: ${h3}"
        fi
    else
        round_trip_unavailable "curl has no HTTP/3"
    fi
    kill -TERM "${pid}" 2>/dev/null || true
    wait "${pid}" 2>/dev/null || true
    echo "template-smoke: ${label} round trip ok (TypeScript client over TCP, HTTP/3 on port ${port})"
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
        check_round_trip service
        check_variant service-pg-otel --template service --db pg --otel
        check_variant service-jobs-console --template service --jobs --console
        check_variant service-pg-otel-jobs-console --template service --db pg --otel --jobs --console
        check_variant service-admin --template service
    fi
done
if ((docker)); then
    check_docker
fi
