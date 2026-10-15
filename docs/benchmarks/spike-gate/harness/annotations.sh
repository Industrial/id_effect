#!/usr/bin/env bash
cd /data/Code/rust/id_effect
OUT=docs/benchmarks/spike-gate
for s in a b; do
    bash spikes/scripts/count-annotations.sh --json "spikes/kernel_$s/tests/corpus.rs" >"$OUT/annotations-$s.json"
done
bash spikes/scripts/count-annotations.sh --json spikes/kernel_b/tests/extension_generic.rs >"$OUT/annotations-b-extension_generic.json"
{
    for f in spikes/kernel_a/tests/corpus.rs spikes/kernel_b/tests/corpus.rs; do
        echo "$f derive_env_structs=$(grep -c '^#\[derive(.*Env' $f) has_bound_terms=$(grep -o 'Has<' $f | wc -l) caps_type_aliases=$(grep -c -E '^\s*(pub )?type .*caps!' $f) caps_macro_uses=$(grep -o 'caps!(' $f | wc -l) turbofish=$(grep -o '::<' $f | wc -l) map_err_calls=$(grep -o 'map_err(' $f | wc -l) lines=$(wc -l <$f)"
    done
} >"$OUT/declarations.txt"
cat "$OUT/declarations.txt"
du -sh "$OUT"; ls -la "$OUT" "$OUT/raw" | head -50
