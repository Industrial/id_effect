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
| 0.11.x | Yes: all fixes, including security fixes |
| 0.10.x | Security fixes only (high and critical), until 2027-01-17 (90 days after the 0.11.0 release); use 0.10.1 or later (see [Security releases](#security-releases)) |
| 0.9.x | Security fixes only, until 2027-01-16 |
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

## Security releases

Public history of releases that fix vulnerabilities. Finding ids (H high, M medium, L low) are
those of the 2026-10-18 security review; [THREAT-MODEL.md](THREAT-MODEL.md) tracks every finding
and its status (closed with its commit, or accepted with its reason).

### 0.10.1 (2026-10-18)

Upgrade from 0.10.0 to 0.10.1 ([CHANGELOG](CHANGELOG.md), "0.10.1 — Security patch"; migration:
"0.10.1: stricter security defaults" in [docs/MIGRATION-0.10.md](docs/MIGRATION-0.10.md)). Every
node of a mesh cluster must be upgraded together (handshake protocol version 2).

| Finding | Crate | Severity | Issue fixed | Advisory |
|---|---|---|---|---|
| H1 | `id_effect_axum` | High | Serve functions dropped the peer address, so per-IP rate limits shared one bucket. | pending |
| H2 | Ledger reference app | High | The login limiter trusted the first `X-Forwarded-For` entry. | not a published crate |
| H3 | `id_effect_node` | High | QUIC accepted any server certificate. | pending |
| H4 | `id_effect_node` | High | The handshake proof could be relayed, reflected or replayed. | pending |
| H5 | `id_effect_auth` | High | No login throttle on password login. | pending |
| H6 | `id_effect_auth` | High | No TOTP lockout after wrong codes. | pending |
| H7 | `id_effect_auth` | High | A `JwtVerifier` without issuer or audience accepted any token. | pending |
| M1 | `id_effect_axum` | Medium | No header-read timeout, connection cap or keep-alive limit. | pending |
| M2 | `id_effect_platform` | Medium | The HTTP client read a body past `max_body_bytes`. | pending |
| M9 | `id_effect_node` | Medium | No handshake timeout or pre-authentication frame cap; short cookies accepted. | pending |
| M15 | `id_effect_auth` | Medium | JWKS fetch was unbounded, followed redirects and allowed plain http; short HMAC keys accepted. | pending |
| M20 | `id_effect_auth` | Medium | Password hashing was unbounded. | pending |
| L4 | `id_effect_auth` | Low | `BasicAuth` hashed passwords of any length. | pending |

"pending": as of 2026-10-19 no GitHub Security Advisory for 0.10.1 is published
([advisories](https://github.com/Industrial/id_effect/security/advisories)). Each row gets its
GHSA id and link when the advisory is published (step 3 of "Advisory publication").

## Supply chain

- `cargo deny check` ([deny.toml](deny.toml)) runs in CI on every pull request and push to `main`:
  RustSec advisories, the license allow-list, banned crates and allowed sources. Every ignored
  advisory carries a written reason in `deny.toml`.
- `cargo audit` runs daily ([.github/workflows/audit.yml](.github/workflows/audit.yml)).
- CI is pinned: every action by full commit SHA, the toolchain by version, `Cargo.lock` committed
  and builds `--locked`; workflows run with read-only default permissions
  (`scripts/check-workflows.sh` enforces this). Pull requests need a DCO sign-off
  ([CONTRIBUTING.md](CONTRIBUTING.md)).
- From 0.11.0 a release is a **signed annotated tag**; the publish workflow
  ([.github/workflows/publish.yml](.github/workflows/publish.yml),
  [docs/RELEASING.md](docs/RELEASING.md)) refuses a lightweight or unverified one. It packages
  every crate with `cargo package --locked`, writes one CycloneDX SBOM per crate, and signs
  **SLSA Build L3** provenance over those `.crate` and `.cdx.json` files with the
  [slsa-github-generator](https://github.com/slsa-framework/slsa-github-generator) (keyless,
  Sigstore). It publishes the same files to crates.io with **trusted publishing** (a short-lived
  OIDC-exchanged token; no workflow reads a stored registry token), checks each upload against
  its attested sha256, then downloads every crate from crates.io and runs `slsa-verifier` on it
  before it creates the GitHub Release. The release carries the `.crate` files, the SBOMs
  (`<crate>-<version>.cdx.json`) and the provenance (`id_effect-<version>.intoto.jsonl`). Verify a
  crate yourself with:

  ```bash
  curl -fsSLO https://static.crates.io/crates/id_effect/id_effect-<version>.crate
  gh release download v<version> -R Industrial/id_effect -p 'id_effect-<version>.intoto.jsonl'
  slsa-verifier verify-artifact id_effect-<version>.crate \
    --provenance-path id_effect-<version>.intoto.jsonl \
    --source-uri github.com/Industrial/id_effect --source-tag v<version>
  ```

  Releases 0.8.0 to 0.10.x carry GitHub build provenance attestations instead (verify those with
  `gh attestation verify <crate>-<version>.crate -R Industrial/id_effect`); they attest crates
  re-downloaded from crates.io, not the files CI built (finding S5,
  [THREAT-MODEL.md](THREAT-MODEL.md)).
