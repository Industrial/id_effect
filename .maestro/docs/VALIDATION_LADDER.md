# Validation Ladder

The harness-experimental project models verification as a 7-rung ladder. Maestro's canonical verification protocol (`maestro-verify`) covers all 7 rungs but groups them under 6 steps.

## The 7-Rung Ladder

1. **Format** — code formatting checks (prettier, etc.)
2. **Lint** — static analysis (eslint, architecture lint, etc.)
3. **Type** — type checking (`tsc --noEmit`, etc.)
4. **Integration** — integration tests
5. **E2E** — end-to-end tests, compiled-binary tests
6. **Platform** — platform-specific tests, deploy readiness
7. **Release** — final verdict, release checks

## Mapping to `maestro-verify`

- **Plan** → Pre-validation (read spec, contracts, prior evidence)
- **Implement** → Code changes
- **Verify** → Rungs 1–5 (format / lint / type / integration / e2e)
- **ProofMap** → Evidence coverage check
- **Verdict** → Rungs 6–7 (platform / release)
- **Branch** → Action based on verdict (merge, rollback, retry)

## Harness-Specific Validation

For `harness-improvement` work types, additional checks apply:

- Policy schema validation via `maestro policy check`
- Skill self-tests via `bun run check:bundled-skills` and `bun run check:skills`
- Contract amendment evidence when contracts change
- One `harness-delta` evidence row per task that touched `.maestro/`, `policies/`, `skills/`, or `hooks/`
