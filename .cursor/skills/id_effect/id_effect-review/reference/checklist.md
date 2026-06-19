# Extended review checklist

## Schema boundaries

- [ ] External input parsed with schema, not raw `serde_json::Value` in domain
- [ ] Parse errors mapped to typed `E` before business logic

## Integration edge

- [ ] `main` / Axum router is only place that calls `run_with` with Live providers
- [ ] CLI uses `exit_code_for_exit` / documented exit code mapping
- [ ] Config loaded via `id_effect_config` providers

## Examples & book

- [ ] New public API has numbered example or book section
- [ ] mdbook snippets compile (`mdbook test` if configured)

## Migration footguns

- [ ] No `service_key!` / HList context remnants
- [ ] No `Effect::provide` in new code
- [ ] Async fn migration follows `appendix-b-migration.md` patterns
