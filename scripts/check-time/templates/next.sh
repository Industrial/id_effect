# shellcheck shell=bash
# gen-caps template "next": the 0.7 kernel (`id_effect::next`) in the recommended style
# (`fn h() -> Effect<_, _, caps_next!(..)>` + `effect_next!({ ~CapNN.0 + .. + ~callee() })`), the
# successor of the spike A template. A composite handler binds its callee with `~`, which narrows
# the caller's env to the callee's (an `Embed` with inferred indices). The edge builds the app env
# once and runs every handler with `.provide_env(env.clone()).block_on()` (another `Embed`, from
# the full env). ID_EFFECT_PATH selects the crate (default: this checkout).
# Interface: the gen-caps.sh header. Sourced by gen-caps.sh; not executable.

tpl_dependencies() {
    printf 'id_effect = { path = "%s" }\n' "${ID_EFFECT_PATH}"
}

tpl_prelude() {
    printf 'use id_effect::next::{Effect, caps_next, effect_next, hlist};\n'
    printf 'use id_effect::{Exit, Never};\n\n'
    printf 'fn ok(exit: Exit<u64, Never>) -> u64 {\n'
    printf '    match exit {\n        Exit::Success(v) => v,\n'
    printf '        Exit::Failure(c) => panic!("handler failed: {c:?}"),\n    }\n}\n\n'
}

# tpl_cap NAME INDEX
tpl_cap() {
    printf '#[derive(Clone, Debug)]\npub struct %s(pub u64);\n\n' "$1"
}

# tpl_env CAP...
tpl_env() {
    local c i=0 sep=""
    printf 'pub type AppEnv = caps_next!(%s);\n\n' "$(IFS=,; echo "$*" | sed 's/,/, /g')"
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
    printf 'pub fn %s() -> Effect<u64, Never, caps_next!(%s)> {\n    effect_next!({ ' "${name}" "$(IFS=,; echo "$*" | sed 's/,/, /g')"
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
        printf '    total += ok(%s().provide_env(env.clone()).block_on());\n' "${h}"
    done
    printf '    total\n}\n\n'
}
