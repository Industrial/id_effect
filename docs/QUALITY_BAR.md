# Quality bar

Per-tier requirements for workspace crates. Tiers are defined in
[ADR 0010](adrs/0010-crate-tiers.md). Each crate declares its tier in
`[package.metadata.id_effect] tier = "…"`.

Run the checker:

```bash
bash scripts/tier-conformance.sh            # table for every package; exit 1 on any FAIL
bash scripts/tier-conformance.sh id_effect  # one or more packages by name
```

## Per-crate checks (automated)

| # | Check | core | runtime | integration | maintenance | labs | merge / example |
|---|---|---|---|---|---|---|---|
| T | Tier declared in manifest | ✔ | ✔ | ✔ | ✔ | ✔ | ✔ |
| R | `README.md` with a ```` ```rust ```` example | ✔ | ✔ | ✔ | — | — | — |
| E | At least one example under `examples/` | ✔ | ✔ | ✔ | — | — | — |
| D | `#![deny(missing_docs)]` in the crate root | ✔ | ✔ | ✔ | — | — | — |
| A | No crate-wide `#![allow(dead_code)]` / `#![allow(unused…)]` | ✔ | ✔ | ✔ | ✔ | — | — |
| S | No `todo!` / `unimplemented!` / "stub" / "spike" in non-test `src/` | ✔ | ✔ | ✔ | — | — | — |
| N | Test density: test attributes per non-blank `src/` line | ≥ 1/40 | ≥ 1/40 | ≥ 1/80 | — | — | — |
| L | Lives under `labs/` and has `publish = false` | — | — | — | — | ✔ | — |

`merge` and `example` packages are reported but not failed. A `merge` crate should
not exist at the end of the consolidation mission.

## Workspace checks (CI)

These hold for the whole workspace rather than per crate:

- **MSRV:** `[workspace.package] rust-version` is inherited by every crate, and the
  CI `msrv` job builds on that toolchain.
- **Lints:** `cargo clippy --workspace --all-targets -- -D warnings`.
- **Tests:** `cargo nextest run --workspace`.
- **Coverage:** ≥ 95 % lines/regions/functions on core, runtime and integration
  crates (`scripts/llvm-cov-nextest.sh`). Labs crates are excluded.
- **Packaging:** `scripts/validate-crate-packaging.sh` (publish order, versions).
- **Minimal versions:** lock resolves and builds with `-Z minimal-versions`.

## Judgement checks (review)

Some things a script can't verify. Reviewers check them for core, runtime and
integration crates:

- The public API has no duplicate of a concept that lives elsewhere in the
  workspace (see the overlap list in ADR 0010).
- Integration tests **fail** rather than silently skip when CI provides their
  backing service.
- Docs (README, book chapter, skill) describe what the code does today. A wrong
  claim is a bug.
- Breaking changes ship a `#[deprecated]` shim for one minor release and a
  CHANGELOG migration note.

## Known heuristic limits

- Check S is a text search. A legitimate use of the word "stub" in a doc comment
  will fail it. Reword the comment or move the code to `#[cfg(test)]`.
- Check N counts `#[test]`, `#[tokio::test]`, `#[rstest]` and `proptest!`
  occurrences. It is a coarse floor, not a measure of test quality.
