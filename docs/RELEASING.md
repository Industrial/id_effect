# Releasing id_effect

A release is a signed `v*` tag. [`.github/workflows/publish.yml`](../.github/workflows/publish.yml)
does the rest: it tests, packages, attests, publishes to crates.io, verifies what crates.io
serves against the attestation and creates the GitHub Release.

## Cutting a release

1. Bump `[workspace.package] version` in the root `Cargo.toml` and `version` in
   `lints/id_effect_lints/Cargo.toml` (the Dylint library is its own workspace). Update
   `CHANGELOG.md` and the `docs/MIGRATION-<minor>.md` guide.
2. `bash scripts/validate-crate-packaging.sh` (publish order and packaging) and
   `cargo build --workspace --locked` (the committed `Cargo.lock` is current).
3. Merge to `main`, then tag the merge commit and push the tag; GitHub Actions does the release:

   ```bash
   git tag -a v0.12.0 -m "v0.12.0"
   git push origin v0.12.0
   ```

To rehearse, run the workflow by hand (Actions → Publish to crates.io → Run workflow) with
**Dry run** checked. A dry run packages and attests everything, runs
`cargo publish --dry-run` over the whole order, checks the packages against the attested digests
and runs `slsa-verifier` on them; it uploads nothing and creates no release. A run without
**Dry run** must be dispatched on a tag ref.

## What the workflow does

| Job | What it does |
|-----|--------------|
| `verify-versions` | The tag equals every publishable version; the tag is annotated and its signature verified. |
| `test` | `cargo nextest run --workspace --locked`. |
| `validate-packaging` | `scripts/validate-crate-packaging.sh`: publish levels, path-only dev-deps, `cargo package` per crate. |
| `build` | `cargo package --locked` of every crate in `scripts/validate-crate-packaging.sh --print-order` (one invocation, so unpublished siblings resolve locally) and of `lints/id_effect_lints`; one CycloneDX SBOM per crate; the sha256 of each file is the provenance subject list. |
| `provenance` | SLSA Build L3 provenance over those files: the [slsa-github-generator](https://github.com/slsa-framework/slsa-github-generator) generic reusable workflow, signed keylessly with Sigstore. |
| `publish` | `cargo publish --locked` crate by crate in the same order, then `id_effect_lints` with its pinned nightly. `.github/scripts/publish-crate.sh` skips a version the crates.io index already has, so re-running the workflow after a partial publish resumes at the first missing crate; after each upload it checks that the `.crate` cargo uploaded has the attested sha256 and stops the release otherwise. |
| `verify` | Downloads every crate from `static.crates.io`, checks its sha256 equals the subject digest in the signed provenance and runs `slsa-verifier verify-artifact` (`s5_attested_digest_matches_published_crate`). Any mismatch fails the run and no release is created. |
| `release` | GitHub Release with the `.crate` files, the `.cdx.json` SBOMs and `id_effect-<version>.intoto.jsonl`. |

`cargo package` is byte-reproducible for the same tree, `Cargo.lock` and cargo version, which is
why publishing "the same tree" uploads the attested bytes: the workflow pins one toolchain
(`RUST_TOOLCHAIN`) for every job and packages with `--locked`. A packaged crate embeds a
`Cargo.lock` with the checksums of its sibling crates, so if a re-run meets a sibling that an
earlier run published from different bytes (a different cargo or lockfile), the per-crate check
in `publish` fails rather than releasing an unverifiable crate; bump the patch version in that case.

All actions are pinned by commit SHA. The one exception is the SLSA generator, a reusable
workflow that must be referenced by its `@vX.Y.Z` tag (it verifies its own ref).

### Verifying a release yourself

```bash
curl -fsSLO https://static.crates.io/crates/id_effect/id_effect-0.11.0.crate
gh release download v0.11.0 -R Industrial/id_effect -p 'id_effect-0.11.0.intoto.jsonl'
slsa-verifier verify-artifact id_effect-0.11.0.crate \
  --provenance-path id_effect-0.11.0.intoto.jsonl \
  --source-uri github.com/Industrial/id_effect --source-tag v0.11.0
```

## crates.io authentication

The `publish` job publishes with the `CARGO_REGISTRY_TOKEN` secret of the `crates-io`
environment, as up to 0.10.1; `publish-crate.sh` hands it to cargo with `cargo login` on stdin.
The token also publishes a crate's first version, so a new crate needs no owner step.

### Owner steps (D15)

Not needed for the token flow above: they belong to the signed-tag and trusted-publishing flow
of 0.11.0, which 0.12.0 replaced with the 0.10.1 flow.

These need the owner's own crates.io and GitHub logins; the agent does not do them. Record the
date of each in the r11-sc-publish task evidence.

| # | Step | Done (date) |
|---|------|-------------|
| 1 | **Register a signing key.** Every tag up to v0.10.x is unsigned or lightweight, and the workflow refuses those. Add the GPG or SSH key you sign with under GitHub → Settings → SSH and GPG keys as a *signing* key, set `git config user.signingkey` (plus `gpg.format ssh` for an SSH key) and tag with `git tag -s`. | |
| 2 | **Reserve `id_effect_lints`** (new in 0.11.0; every other crate in the order is already on crates.io). From your own machine, with a crates.io API token scoped to `publish-new` for that one name and a one-day expiry, publish a placeholder `0.0.0` (a minimal crate with the same name, `description`, `license = "MIT"` and `repository`), then yank it (`cargo yank --version 0.0.0 id_effect_lints`) and revoke the token. Repeat for any later new crate before its first release. | |
| 3 | **Add the trusted publisher on every crate**: each crate in `scripts/validate-crate-packaging.sh --print-order`, plus `id_effect_lints`. crates.io → the crate → Settings → Trusted Publishing → Add → GitHub: repository owner `Industrial`, repository name `id_effect`, workflow filename `publish.yml`, environment `crates-io`. | |
| 4 | **Protect the `crates-io` environment** (it exists, with no deployment rule). GitHub → Settings → Environments → `crates-io`: deployment refs `v*` tags plus the `release/*` branches dry runs are dispatched from, optionally yourself as required reviewer. | |
| 5 | **Rehearse:** Actions → Publish to crates.io → Run workflow, **Dry run** checked (no token is requested). Put the run URL in the evidence. | |
| 6 | **Release 0.11.0** with the signed tag. The `crates.io token (trusted publishing, …)` steps succeed and `verify` reports every crate. | |
| 7 | **After that first trusted publish succeeds,** delete the `CARGO_REGISTRY_TOKEN` repository secret (GitHub → Settings → Secrets and variables → Actions) and revoke that token on crates.io (Account Settings → API Tokens). Nothing reads it any more; it is kept until then only as the rollback below. | |

Rollback: the previous workflow (long-lived token plus `actions/attest-build-provenance`) is one
revert of `publish.yml` and `.github/scripts/publish-crate.sh` away, and the secret is only
deleted (step 7) after a trusted publish has worked.
