# shellcheck shell=bash
# gen-caps template "b": kernel spike B (trait-bound env), B1 style as the design recommends —
# generic handlers `fn h<R: Has<CapA> + Has<CapB>>() -> Effect<u64, (), R>` with `~Cap` reads
# inside `effect!`, a composite handler binds its callee with `~ callee()` (bound entailment, no
# narrowing), and the edge builds one `#[derive(Env)]` struct over every cap.
# Interface: spikes/README.md, "gen-caps templates". Sourced by gen-caps.sh; not executable.

kb_field() {
    printf '%s' "${1,,}"
}

tpl_dependencies() {
    printf 'kernel_b = { path = "%s/kernel_b" }\n' "${SPIKES_DIR}"
}

tpl_prelude() {
    printf 'use kernel_b::{Effect, Has, effect, run_blocking};\n\n'
}

# tpl_cap NAME INDEX
tpl_cap() {
    printf '#[derive(Clone, Debug)]\npub struct %s(pub u64);\n\n' "$1"
}

# tpl_env CAP...
tpl_env() {
    local c
    printf '#[derive(Clone, kernel_b::Env)]\npub struct AppEnv {\n'
    for c in "$@"; do
        printf '    pub %s: %s,\n' "$(kb_field "${c}")" "${c}"
    done
    printf '}\n\n'
}

# tpl_handler NAME CALLEE|- CAP...
tpl_handler() {
    local name=$1 callee=$2 c sep=""
    shift 2
    printf 'pub fn %s<R: ' "${name}"
    for c in "$@"; do
        printf '%sHas<%s>' "${sep}" "${c}"
        sep=" + "
    done
    printf '>() -> Effect<u64, (), R> {\n    effect!(|r| {\n        let mut sum = 0;\n'
    for c in "$@"; do
        printf '        sum += ~%s.0;\n' "${c}"
    done
    [[ "${callee}" == - ]] || printf '        sum += ~ %s();\n' "${callee}"
    printf '        sum\n    })\n}\n\n'
}

# tpl_edge HANDLER...  (the cap list is in CAP_NAMES)
tpl_edge() {
    local h i
    printf 'pub fn edge() -> u64 {\n    let env = AppEnv {\n'
    for ((i = 0; i < ${#CAP_NAMES[@]}; i++)); do
        printf '        %s: %s(%d),\n' "$(kb_field "${CAP_NAMES[${i}]}")" "${CAP_NAMES[${i}]}" "${i}"
    done
    printf '    };\n    let mut total = 0;\n'
    for h in "$@"; do
        printf '    total += run_blocking(%s(), env.clone()).expect("%s");\n' "${h}" "${h}"
    done
    printf '    total\n}\n\n'
}
