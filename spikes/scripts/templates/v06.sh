# shellcheck shell=bash
# gen-caps template "v06": id_effect 0.6 handlers in the documented axum style
# (`fn h(r: &mut Env) -> Effect<_, _, Env>` + `Needs::<T>::need(r)`, cf. axum example
# 020_capability_run_with). This is the "0.6 equivalent" Design A's compile-time falsifier
# (> 2x) is measured against. ID_EFFECT_PATH selects the crate (default: this checkout).
# Interface: spikes/README.md, "gen-caps templates". Sourced by gen-caps.sh; not executable.

tpl_dependencies() {
    printf 'id_effect = { path = "%s" }\n' "${ID_EFFECT_PATH}"
}

tpl_prelude() {
    printf 'use id_effect::{Cap, Effect, Env, Needs, run_blocking, succeed};\n\n'
}

# tpl_cap NAME INDEX
tpl_cap() {
    printf '#[derive(Clone, Debug)]\npub struct %s(pub u64);\n\n' "$1"
}

# tpl_env CAP...
tpl_env() {
    local c i=0
    printf 'pub fn app_env() -> Env {\n    let mut env = Env::new();\n'
    for c in "$@"; do
        printf '    env.insert::<Cap<%s>>(%s(%d));\n' "${c}" "${c}" "${i}"
        i=$((i + 1))
    done
    printf '    env\n}\n\n'
}

# tpl_handler NAME CALLEE|- CAP...
tpl_handler() {
    local name=$1 callee=$2 c sep=""
    shift 2
    printf 'pub fn %s(r: &mut Env) -> Effect<u64, (), Env> {\n    let sum = ' "${name}"
    for c in "$@"; do
        printf '%sNeeds::<%s>::need(r).0' "${sep}" "${c}"
        sep=" + "
    done
    printf ';\n'
    if [[ "${callee}" == - ]]; then
        printf '    succeed(sum)\n}\n\n'
    else
        printf '    %s(r).map(move |v| v + sum)\n}\n\n' "${callee}"
    fi
}

# tpl_edge HANDLER...
tpl_edge() {
    local h
    printf 'pub fn edge() -> u64 {\n    let env = app_env();\n    let mut total = 0;\n'
    for h in "$@"; do
        printf '    total += run_blocking(%s(&mut env.clone()), env.clone()).expect("%s");\n' "${h}" "${h}"
    done
    printf '    total\n}\n\n'
}
