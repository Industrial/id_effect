# Fuzzing

id_effect fuzzes the public decoders that read untrusted input with
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) and runs them in CI with
[ClusterFuzzLite](https://google.github.io/clusterfuzzlite/) (0.11.0 decision D20).

## Targets

The targets live in `fuzz/fuzz_targets/`, one file per decoder. Each one reaches its crate through
the public API only, and checks a documented property besides "does not panic".

| Target | Decoder (crate) | Property checked |
|---|---|---|
| `schema_json` | JSON → `Unknown` → `Schema::decode_unknown` / `decode_unknown_all` (`id_effect`, feature `schema-serde`) | both decoders agree; a failure has an issue; decoded values round-trip through `encode` |
| `route_path` | path parameters through a `Router` into `route::ValidPath` (`id_effect_axum`) | never a 5xx |
| `route_query` | the query string into `route::ValidQuery` (`id_effect_axum`) | an `Invalid` rejection has an issue and a 4xx status |
| `route_form` | urlencoded, multipart and `GET` forms into `route::ValidForm` (`id_effect_axum`) | as `route_query` |
| `cron_parse` | `Cron::parse`, `next_after`, `upcoming` (`id_effect`) | the text re-parses; in UTC the next fire is later and matches |
| `config_profile_toml` | `base.toml` + `prod.toml` merged by `Profiles::load_for` (`id_effect_config`) | a merged document extracts |
| `jwt_decode` | `JwtVerifier::verify`: header, signature, claims, claim mapping (`id_effect_auth`, feature `jwt`) | an unsigned fuzzed token is never accepted |
| `jwks_parse` | a JWKS document through `Jwk` and `JwtKey::from_jwk`, as `JwksClient` reads one (`id_effect_auth`) | usable keys verify tokens of every family without panicking |
| `totp_otpauth` | base32 secret, `otpauth://` URI label and typed code (`id_effect_auth`, feature `totp`) | base32 round-trips; the URI carries the secret; the current code verifies once, then is a replay |

Two inputs need a format the fuzzer can reach:

- `config_profile_toml`, `jwt_decode`: a `\n---\n` line splits the input into two documents (the
  two TOML files; the JWT header and claims, which the target base64url-encodes and signs with
  the verifier's HS256 secret, so the claims checks see fuzzed JSON behind a valid signature).
- `route_query`, `route_form`: the first byte picks the content type and the schema.

`JwksClient` fetches over HTTP and its parser is private, so `jwks_parse` drives the public steps
that parser is made of. id_effect_auth builds `otpauth` URIs but parses none, so `totp_otpauth`
covers the parts of enrolment and login that come from outside.

## Running locally

The fuzz crate is its own workspace outside the main one. It builds on one pinned nightly,
`fuzz/rust-toolchain.toml` (the same date `devenv.nix` pins), so rustup picks it up inside `fuzz/`:

```sh
cargo install cargo-fuzz --locked
cd fuzz
cargo fuzz list
cargo fuzz build -O --debug-assertions
cargo fuzz run cron_parse -- -max_total_time=60
# every target for a minute each (the leaf gate):
for t in $(cargo fuzz list); do cargo fuzz run "$t" -- -max_total_time=60; done
```

With rustup the toolchain file selects the nightly (or name it: `cargo +nightly-2026-07-10 fuzz
build`); in the devenv shell `cargo` already is that nightly. `cargo fuzz run <target>` reads and
grows `fuzz/corpus/<target>/`; only the `seed-*` files there are committed (`fuzz/.gitignore`),
the rest is your local corpus.

## In CI

- **`.github/workflows/cflite_pr.yml`** runs on pull requests that touch `crates/`, `fuzz/`,
  `.clusterfuzzlite/` or `Cargo.lock`. It builds the targets with AddressSanitizer and fuzzes
  them for 5 minutes each (`fuzz-seconds` is split across the targets: raise it by 300 for each
  target you add), starting from the stored corpus. A crash the base branch does not have fails
  the job.
- **`.github/workflows/cflite_batch.yml`** runs nightly (and on demand): 10 minutes per target,
  then corpus pruning. The corpus is stored as workflow artifacts, which the next batch and the
  PR job download.
- `.clusterfuzzlite/Dockerfile` builds on `base-builder-rust` (pinned by digest) with the pinned
  nightly; `.clusterfuzzlite/build.sh` copies every `cargo fuzz list` target to `$OUT` with its
  seeds as `<target>_seed_corpus.zip`.

Bump the nightly in `fuzz/rust-toolchain.toml` and `.clusterfuzzlite/Dockerfile` together.

## When a target crashes

1. **Get the input.** CI uploads the crashing testcase as an artifact of the failing run (the log
   names the target and shows the panic or sanitizer report). Locally libFuzzer writes it to
   `fuzz/artifacts/<target>/crash-<sha1>`.
2. **Reproduce and minimise.** `cargo fuzz run <target> fuzz/artifacts/<target>/crash-…`, then
   `cargo fuzz tmin <target> fuzz/artifacts/<target>/crash-…`.
3. **Open an issue.** A crash in a decoder that reads untrusted input is a denial of service at
   least: follow `SECURITY.md` (private advisory) when the input is reachable from the network,
   otherwise a normal issue labelled `fuzzing` with the target, the minimised input (base64 if it
   is binary) and the report.
4. **Fix it with a regression test.** The fix lands in the crate that owns the parser, with a unit
   or integration test there that feeds the minimised input and asserts the error the decoder
   should give. Also commit the input as `fuzz/corpus/<target>/seed-crash-<issue>` so every later
   fuzz run starts from it.
5. **Close the issue** once the PR job is green with the new seed.
