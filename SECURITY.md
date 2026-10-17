# Security policy

## Reporting a vulnerability

Report vulnerabilities **privately** through GitHub's private vulnerability reporting:
[Security → Report a vulnerability](https://github.com/Industrial/id_effect/security/advisories/new).
This opens a draft GitHub Security Advisory (GHSA) that only the maintainers and you can see.

Do not open a public issue, pull request or discussion for a suspected vulnerability.

Please include the affected crate(s) and version(s), a description of the impact, and a minimal
reproduction if you have one.

## Response times

id_effect has one maintainer (see [GOVERNANCE.md](GOVERNANCE.md)), so these are targets, not a
contractual SLA:

| Step | Target |
|---|---|
| Acknowledge the report | within 7 days |
| Initial assessment (valid or not, severity) | within 14 days |
| Fix released for a confirmed issue | within 90 days of the report; sooner for high or critical severity |

If a fix needs longer, we tell you why and agree a new disclosure date with you. We credit
reporters in the advisory unless you ask us not to.

## Supported versions

Only published crates in the main workspace are covered (`crates/*`). The unpublished `labs/`
crates and the `examples/` are not.

| Version | Supported |
|---|---|
| 0.9.x | Yes: all fixes, including security fixes |
| 0.8.x | Security fixes only, until 2027-01-15 |
| 0.7.x | Security fixes only, until 2027-01-14 |
| 0.6.x | Security fixes only, until 2027-01-13 |
| < 0.6 | No |

### Support window

- The **latest minor** release line receives all fixes as patch releases.
- The **previous minor** receives fixes for high and critical vulnerabilities for **90 days**
  after the newer minor is released. Then it is unsupported.
- Older lines are unsupported: upgrade using the migration guides in [`docs/`](docs/).

The table above is updated with each minor release. The 1.0 release will define a longer-term
support policy ([docs/STABILITY.md](docs/STABILITY.md)).

## Advisory publication

For a confirmed vulnerability:

1. We work on the fix in the private GHSA (with a private fork if needed) and invite you to it.
2. We request a CVE through the GHSA when the issue warrants one.
3. We release patched versions of every supported line affected, then publish the GHSA.
4. We submit the advisory to the [RustSec advisory database](https://github.com/rustsec/advisory-db)
   so `cargo audit` and `cargo deny` users are warned, referencing the GHSA (and CVE).
5. The CHANGELOG entry for the patch release names the advisory. Affected versions may be
   yanked from crates.io if they are dangerous to keep using.

## Supply chain

- `cargo deny check` ([deny.toml](deny.toml)) runs in CI on every pull request and push to `main`:
  RustSec advisories, the license allow-list, banned crates and allowed sources. Every ignored
  advisory carries a written reason in `deny.toml`.
- `cargo audit` runs daily ([.github/workflows/audit.yml](.github/workflows/audit.yml)).
- From 0.8.0, each release attaches a CycloneDX SBOM per crate (`<crate>-<version>.cdx.json`) and the
  published `.crate` files to the GitHub release, with build provenance attestations. Verify a
  downloaded crate with:

  ```bash
  gh attestation verify id_effect-<version>.crate -R Industrial/id_effect
  ```
