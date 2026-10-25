# Contributing to id_effect

Thank you for helping. This page covers the legal part; how work is planned and reviewed is in
[GOVERNANCE.md](GOVERNANCE.md), and the quality gates every change must pass are in
[docs/QUALITY_BAR.md](docs/QUALITY_BAR.md).

## License of contributions

id_effect is licensed under the [MIT License](LICENSE) from 0.8 onwards. By contributing, you
agree that your contribution is licensed under the MIT License as well.

## Developer Certificate of Origin (DCO)

Every commit must be signed off. A sign-off certifies the
[Developer Certificate of Origin 1.1](https://developercertificate.org/): that you wrote the change
or otherwise have the right to submit it under the project's license. Add it with
`git commit -s`, which appends:

```text
Signed-off-by: Your Name <you@example.com>
```

Use your real name and an email address you can be reached at. Pull requests with commits
lacking a sign-off are not merged.

The [DCO workflow](.github/workflows/dco.yml) enforces this on every pull request: each non-merge
commit the pull request adds needs a `Signed-off-by:` trailer with the commit author's email.
Commits that predate the workflow (0.11.0) are not checked, and commits authored by
`dependabot[bot]` are exempt. To fix a failing check, sign off and force-push:

```bash
git commit --amend -s --no-edit        # the last commit
git rebase --signoff origin/main       # every commit of the branch
```

`main` only accepts squash merges; keep the `Signed-off-by:` lines in the squash message.

## Signed commits and tags

A sign-off is a statement; a signature proves who made the commit. Sign your commits with one of:

- **SSH** (simplest if you already push over SSH):
  `git config gpg.format ssh`, `git config user.signingkey ~/.ssh/id_ed25519.pub`,
  `git config commit.gpgsign true`, and add the key to GitHub as a *signing* key.
- **GPG**: `git config user.signingkey <key id>`, `git config commit.gpgsign true`, and add the
  public key to GitHub.
- **[gitsign](https://github.com/sigstore/gitsign)** (keyless, Sigstore):
  `git config gpg.x509.program gitsign`, `git config gpg.format x509`,
  `git config commit.gpgsign true`.

`git commit -S -s` signs and signs off in one step. Commits GitHub creates (squash merges, the web
editor) are signed by GitHub.

Release tags are annotated tags (`git tag -a vX.Y.Z -m "vX.Y.Z"`); signing them (`git tag -s`) is
welcome but not checked: the publish workflow publishes any pushed `v*` tag whose version matches
the workspace (owner decision 2026-10-24, #118; S6 is an accepted risk,
[docs/RELEASING.md](docs/RELEASING.md)).

Requiring signed commits is a repository setting, not a check in this repository. Owner actions
([0.11.0 D15](.maestro/specs/releases/0.11.0.md)):

| Action | Status |
|---|---|
| "Require signed commits" on `main` and `release/*` | Open. 2026-10-19: the `main` ruleset (required status check "CI Pass", squash merges, linear history) has no signature rule; `release/*` has no ruleset. |
| DCO check "Signed-off-by on every commit" as a required status check | Open (2026-10-19); the workflow runs on every pull request from 0.11.0. |

The owner replaces "Open" with the date each setting is enabled.

## Security issues

Do not open public issues for vulnerabilities; follow [SECURITY.md](SECURITY.md).
