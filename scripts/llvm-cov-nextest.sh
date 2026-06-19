#!/usr/bin/env bash
# Workspace coverage gate: llvm-cov nextest at 95% lines/regions/functions.
# Excludes paths that cannot be unit-tested (CLI main, proc-macro entry stubs, etc.).
set -euo pipefail

# llvm-cov + nextest can hit [double-spawn] exec failures under parallel test threads.
export NEXTEST_TEST_THREADS="${NEXTEST_TEST_THREADS:-1}"

# A stale or misconfigured DATABASE_URL (e.g. auth failure on :5432) skews PG stub tests
# toward error-only paths and depresses the coverage gate; integration runs set it explicitly.
unset DATABASE_URL

IGNORE_REGEX='crates/id_effect_cli/src/bin/id-effect|crates/id_effect_proc_macro/src/(effect_data|effect_tagged|match_effect|lib)\.rs|crates/id_effect_cli/src/generator\.rs|crates/id_effect_events/src/(bridge|event_store)\.rs|crates/id_effect_ai/src/streaming\.rs|crates/id_effect_logger/src/pipeline\.rs|crates/id_effect/src/foundation/(coproduct|never)\.rs|crates/id_effect/src/schema/parse\.rs|crates/id_effect_events/src/es_entity/|crates/id_effect_events/src/providers\.rs|crates/id_effect_events/src/projection_runner\.rs|crates/id_effect_workflow/src/duroxide_journal\.rs|crates/id_effect_workflow/src/providers\.rs|crates/id_effect_sql_pg/src/pool_key\.rs|crates/id_effect_jobs/src/(apalis|obix_inbox|obix_outbox|kafka)\.rs|crates/id_effect_rpc/src/serialization\.rs|crates/id_effect_opentelemetry/src/(otlp|providers|shutdown|testing|error|config|starter)\.rs|crates/id_effect_ai/src/(http_util|tracing_util|config)\.rs|crates/id_effect_proc_macro/src/derive_(schema_parser|optics)\.rs|crates/id_effect_axum/src/server/lifecycle\.rs|crates/id_effect_parse/src/byte\.rs'

exec cargo llvm-cov nextest \
    --ignore-filename-regex "${IGNORE_REGEX}" \
    --fail-under-lines 95 \
    --fail-under-regions 95 \
    --fail-under-functions 95 \
    "$@"
