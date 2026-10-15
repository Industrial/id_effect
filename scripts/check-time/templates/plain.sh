# shellcheck shell=bash
# gen-caps template "plain": no effect kernel, plain Rust functions over a struct env.
# The floor every kernel's check time is read against, and the generator's own self-test.
# Interface: the gen-caps.sh header. Sourced by gen-caps.sh; not executable.

field() {
    printf '%s' "${1,,}"
}

tpl_dependencies() {
    :
}

tpl_prelude() {
    :
}

# tpl_cap NAME INDEX
tpl_cap() {
    printf '#[derive(Clone, Debug)]\npub struct %s(pub u64);\n\n' "$1"
}

# tpl_env CAP...
tpl_env() {
    local c
    printf '#[derive(Clone, Debug)]\npub struct AppEnv {\n'
    for c in "$@"; do
        printf '    pub %s: %s,\n' "$(field "${c}")" "${c}"
    done
    printf '}\n\n'
}

# tpl_handler NAME CALLEE|- CAP...
tpl_handler() {
    local name=$1 callee=$2 c
    shift 2
    printf 'pub fn %s(env: &AppEnv) -> u64 {\n    let mut sum = 0;\n' "${name}"
    for c in "$@"; do
        printf '    sum += env.%s.0;\n' "$(field "${c}")"
    done
    [[ "${callee}" == - ]] || printf '    sum += %s(env);\n' "${callee}"
    printf '    sum\n}\n\n'
}

# tpl_edge HANDLER...  (the cap list is in CAP_NAMES)
tpl_edge() {
    local h i
    printf 'pub fn edge() -> u64 {\n    let env = AppEnv {\n'
    for ((i = 0; i < ${#CAP_NAMES[@]}; i++)); do
        printf '        %s: %s(%d),\n' "$(field "${CAP_NAMES[${i}]}")" "${CAP_NAMES[${i}]}" "${i}"
    done
    printf '    };\n    let mut total = 0;\n'
    for h in "$@"; do
        printf '    total += %s(&env);\n' "${h}"
    done
    printf '    total\n}\n\n'
}
