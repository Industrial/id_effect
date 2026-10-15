#!/usr/bin/env bash
# Workspace coverage gate: llvm-cov nextest at 96% lines/regions/functions.
# Excludes paths that cannot be unit-tested (CLI main, proc-macro entry stubs, etc.).
set -euo pipefail

# llvm-cov + nextest can hit [double-spawn] exec failures under parallel test threads.
export NEXTEST_TEST_THREADS="${NEXTEST_TEST_THREADS:-1}"

# A stale or misconfigured DATABASE_URL (e.g. auth failure on :5432) skews PG stub tests
# toward error-only paths and depresses the coverage gate; integration runs set it explicitly.
unset DATABASE_URL

# Each entry needs a justification; w3-conformance reviews this list (ADR 0010 / QUALITY_BAR).
#   sql/src/pg/{pool_key,client,transaction}: need a live database; exercised by the CI
#     `pg-integration` job (Postgres service, ID_EFFECT_REQUIRE_PG=1), not this unit run.
#   fp/src/foundation/never: every function takes the uninhabited `Infallible`, so its bodies
#     can never execute (moved from id_effect/src/foundation in W2; coproduct.rs is tested).
IGNORE_REGEX='crates/id_effect_proc_macro/src/(effect_data|effect_tagged|match_effect|lib)\.rs|crates/id_effect_logger/src/pipeline\.rs|crates/id_effect_fp/src/foundation/never\.rs|crates/id_effect/src/schema/parse\.rs|crates/id_effect_sql/src/pg/(pool_key|client|transaction)\.rs|crates/id_effect_opentelemetry/src/(otlp|providers|shutdown|testing|error|config|starter)\.rs|crates/id_effect_proc_macro/src/derive_(schema_parser|optics)\.rs|crates/id_effect_axum/src/server/lifecycle\.rs'

exec cargo llvm-cov nextest \
    --ignore-filename-regex "${IGNORE_REGEX}" \
    --fail-under-lines 96 \
    --fail-under-regions 96 \
    --fail-under-functions 96 \
    "$@"
