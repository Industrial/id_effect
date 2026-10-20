#!/bin/bash -eu
# ClusterFuzzLite build: every cargo-fuzz target in fuzz/, with its committed seeds as the seed
# corpus. Runs inside .clusterfuzzlite/Dockerfile, which sets RUSTUP_TOOLCHAIN.

cd "$SRC/id_effect/fuzz"
cargo fuzz build -O --debug-assertions

release="fuzz/target/x86_64-unknown-linux-gnu/release"
for target in $(cargo fuzz list); do
    cp "$SRC/id_effect/$release/$target" "$OUT/"
    if compgen -G "corpus/$target/seed-*" >/dev/null; then
        zip -q -j "$OUT/${target}_seed_corpus.zip" corpus/"$target"/seed-*
    fi
done
