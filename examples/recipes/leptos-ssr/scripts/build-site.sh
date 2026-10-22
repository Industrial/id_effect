#!/usr/bin/env bash
# Build the browser half of the Leptos recipe — no npm, no node_modules:
#
#   1. this crate as a cdylib for wasm32-unknown-unknown with feature `hydrate` (cargo);
#   2. wasm-bindgen-cli binds the module into <site>/pkg/leptos_ssr.js + leptos_ssr_bg.wasm.
#
# Needs the wasm32-unknown-unknown target (`rustup target add wasm32-unknown-unknown`) and
# wasm-bindgen-cli at exactly the wasm-bindgen version in Cargo.lock
# (`cargo install wasm-bindgen-cli --version <it> --locked`); the script checks the version.
#
# Environment:
#   LEPTOS_SSR_SITE_ROOT  output site root (default target/site; the server reads the same var)
#   PROFILE               cargo profile (default release)
#   WASM_BINDGEN          the wasm-bindgen binary (default: wasm-bindgen on PATH)
#   BUILD_STD=1           build std from rust-src (-Zbuild-std; a nightly without the target's
#                         prebuilt std, such as a Nix toolchain)
set -euo pipefail

cd "$(dirname "$0")/.."

site="${LEPTOS_SSR_SITE_ROOT:-target/site}"
profile="${PROFILE:-release}"
wasm_bindgen="${WASM_BINDGEN:-wasm-bindgen}"
target_dir="${CARGO_TARGET_DIR:-target}"

want="$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' Cargo.lock)"
have="$("${wasm_bindgen}" --version 2>/dev/null | awk '{ print $2 }' || true)"
if [ -z "${want}" ]; then
    echo "build-site: no wasm-bindgen in Cargo.lock" >&2
    exit 1
fi
if [ "${have}" != "${want}" ]; then
    echo "build-site: wasm-bindgen-cli ${want} required (found: ${have:-none})." >&2
    echo "  cargo install wasm-bindgen-cli --version ${want} --locked" >&2
    exit 1
fi

build_std=()
if [ "${BUILD_STD:-0}" = "1" ]; then
    build_std=(-Zbuild-std=std,panic_abort)
fi

# A cdylib for this build only (Cargo.toml's [lib] is an rlib for the server).
cargo rustc --locked --lib "${build_std[@]}" \
    --target wasm32-unknown-unknown \
    --no-default-features --features hydrate \
    --profile "${profile}" \
    --crate-type cdylib

case "${profile}" in
    dev) out=debug ;;
    *) out="${profile}" ;;
esac

mkdir -p "${site}/pkg"
"${wasm_bindgen}" --target web --no-typescript \
    --out-dir "${site}/pkg" --out-name leptos_ssr \
    "${target_dir}/wasm32-unknown-unknown/${out}/leptos_ssr.wasm"

ls -l "${site}/pkg"
