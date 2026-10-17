# Quality bar

Per-tier requirements for workspace crates. Tiers are defined in
[ADR 0010](adrs/0010-crate-tiers.md); the `platform` tier is added by
[ADR 0012](adrs/0012-platform-vision.md). Each crate declares its tier in
`[package.metadata.id_effect] tier = "…"`.

Run the checker:

```bash
bash scripts/tier-conformance.sh            # table for every package; exit 1 on any FAIL
bash scripts/tier-conformance.sh id_effect  # one or more packages by name
```

## Per-crate checks (automated)

| # | Check | core | runtime | integration | platform | maintenance | labs | merge / example |
|---|---|---|---|---|---|---|---|---|
| T | Tier declared in manifest (a known tier) | ✔ | ✔ | ✔ | ✔ | ✔ | ✔ | ✔ |
| R | `README.md` with a ```` ```rust ```` example | ✔ | ✔ | ✔ | ✔ | — | — | — |
| E | At least one example under `examples/` | ✔ | ✔ | ✔ | ✔ | — | — | — |
| D | `#![deny(missing_docs)]` in the crate root | ✔ | ✔ | ✔ | ✔ | — | — | — |
| A | No crate-wide `#![allow(dead_code)]` / `#![allow(unused…)]` | ✔ | ✔ | ✔ | ✔ | ✔ | — | — |
| S | No `todo!` / `unimplemented!` / "stub" / "spike" in non-test `src/` | ✔ | ✔ | ✔ | ✔ | — | — | — |
| N | Test density: test attributes per non-blank `src/` line | ≥ 1/40 | ≥ 1/40 | ≥ 1/80 | ≥ 1/80 | — | — | — |
| P | A CI-tested reference app under `examples/reference/` depends on it | — | — | — | ✔ | — | — | — |
| L | Lives under `labs/` and has `publish = false` | — | — | — | — | — | ✔ | — |

`platform` crates carry the integration gates plus check P (ADR 0012): a reference app under
`examples/reference/` depends on the crate, named in its `Cargo.toml` or reached through
another dependency (the apps take the batteries through `id_effect_kit`'s features; the
script reads `cargo tree -p <app>`, dev-dependencies included). That the app runs in CI is a
review check.

`merge` and `example` packages are reported but not failed. A `merge` crate should
not exist at the end of the consolidation mission. As of 0.7 none does (ADR 0010,
Amendment 3). An unknown tier value fails check T.

## Workspace checks (CI)

These hold for the whole workspace rather than per crate:

- **MSRV:** `[workspace.package] rust-version` is inherited by every crate, and the
  CI `msrv` job builds on that toolchain.
- **Lints:** `cargo clippy --workspace --all-targets -- -D warnings`.
- **Tests:** `cargo nextest run --workspace`.
- **Coverage:** ≥ 96 % lines/regions/functions on core, runtime, integration and
  platform crates (`scripts/llvm-cov-nextest.sh`). Labs crates are excluded.
- **Packaging:** `scripts/validate-crate-packaging.sh` (publish order, versions).
- **Minimal versions:** lock resolves and builds with `-Z minimal-versions`.

## Judgement checks (review)

Some things a script can't verify. Reviewers check them for core, runtime,
integration and platform crates:

- The public API has no duplicate of a concept that lives elsewhere in the
  workspace (see the overlap list in ADR 0010).
- Integration tests **fail** rather than silently skip when CI provides their
  backing service.
- Docs (README, book chapter, skill) describe what the code does today. A wrong
  claim is a bug.
- Breaking changes ship a `#[deprecated]` shim for one minor release and a
  CHANGELOG migration note. The full rule, semver and MSRV policy: [STABILITY.md](STABILITY.md).

## Known heuristic limits

- Check S is a text search. A legitimate use of the word "stub" in a doc comment
  will fail it. Reword the comment or move the code to `#[cfg(test)]`.
- Check N counts `#[test]`, `#[tokio::test]`, `#[rstest]` and `proptest!`
  occurrences. It is a coarse floor, not a measure of test quality.
