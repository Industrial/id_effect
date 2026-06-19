#!/usr/bin/env bash
# Workspace coverage gate: llvm-cov nextest at 95% lines/regions/functions.
# Excludes paths that cannot be unit-tested (CLI main, proc-macro entry stubs, etc.).
set -euo pipefail

IGNORE_REGEX='crates/id_effect_cli/src/bin/id-effect|crates/id_effect_proc_macro/src/(effect_data|effect_tagged|match_effect|lib)\.rs|crates/id_effect_cli/src/generator\.rs|crates/id_effect_events/src/(bridge|event_store)\.rs|crates/id_effect_ai/src/streaming\.rs|crates/id_effect_logger/src/pipeline\.rs|crates/id_effect/src/foundation/(coproduct|never)\.rs|crates/id_effect/src/schema/parse\.rs'

exec cargo llvm-cov nextest \
    --ignore-filename-regex "${IGNORE_REGEX}" \
    --fail-under-lines 95 \
    --fail-under-regions 95 \
    --fail-under-functions 95 \
    "$@"
