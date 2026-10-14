# shellcheck shell=bash
# gen-caps template "a": kernel spike A handlers in the design's recommended style
# (`fn h() -> Effect<_, _, caps!(..)>` + `effect!({ ~CapNN.0 + .. + ~callee() })`). A composite
# handler binds its callee with `~`, which narrows the caller's env to the callee's (an `Embed`
# with inferred indices). The edge builds the 16-cap app env once and runs every handler with
# `.provide_env(env.clone())` (another `Embed`, from the full env).
# Interface: spikes/README.md, "gen-caps templates". Sourced by gen-caps.sh; not executable.

tpl_dependencies() {
    printf 'kernel_a = { path = "%s/kernel_a" }\n' "${SPIKES_DIR}"
}

tpl_prelude() {
    printf 'use kernel_a::{Effect, Never, caps, effect, hlist};\n\n'
}

# tpl_cap NAME INDEX
tpl_cap() {
    printf '#[derive(Clone, Debug)]\npub struct %s(pub u64);\n\n' "$1"
}

# tpl_env CAP...
tpl_env() {
    local c i=0 sep=""
    printf 'pub type AppEnv = caps!(%s);\n\n' "$(IFS=,; echo "$*" | sed 's/,/, /g')"
    printf 'pub fn app_env() -> AppEnv {\n    hlist!['
    for c in "$@"; do
        printf '%s%s(%d)' "${sep}" "${c}" "${i}"
        sep=", "
        i=$((i + 1))
    done
    printf ']\n}\n\n'
}

# tpl_handler NAME CALLEE|- CAP...
tpl_handler() {
    local name=$1 callee=$2 c sep=""
    shift 2
    printf 'pub fn %s() -> Effect<u64, Never, caps!(%s)> {\n    effect!({ ' "${name}" "$(IFS=,; echo "$*" | sed 's/,/, /g')"
    for c in "$@"; do
        printf '%s~%s.0' "${sep}" "${c}"
        sep=" + "
    done
    [[ "${callee}" == - ]] || printf ' + ~%s()' "${callee}"
    printf ' })\n}\n\n'
}

# tpl_edge HANDLER...
tpl_edge() {
    local h
    printf 'pub fn edge() -> u64 {\n    let env = app_env();\n    let mut total = 0;\n'
    for h in "$@"; do
        printf '    total += %s().provide_env(env.clone()).block_on().expect("%s");\n' "${h}" "${h}"
    done
    printf '    total\n}\n\n'
}
