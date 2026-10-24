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
| `mesh_frame` | mesh wire frames, `wire::decode` (protocol v3), and the process envelope inside a data frame (`id_effect_node`) | a decoded frame re-encodes to bytes that decode to the same frame; so does the envelope |
| `mesh_envelope` | `decode_envelope`, then `Mesh::handle_envelope_from` on a fresh node: dispatch, the at-least-once dedup window and acks (`id_effect_node`) | a decoded envelope round-trips; a reliable message unwraps to the matching at-most-once envelope |
| `rpc_wire_request` | the JSON envelopes of `POST /rpc`: `WireCall` (as the server decodes it, `CodecId::Json`), `WireResult` and `WireExit` (`id_effect_rpc`, 0.12) | a decoded call keeps its id, tag and version through a round trip; an exit keeps success or failure |
| `rpc_batch` | `wire::decode_batch(bytes, max_calls, max_bytes)`, the batch body of `POST /rpc`, with fuzzed caps and with the server defaults (`id_effect_rpc`) | an accepted batch is non-empty, within both caps, every call has an id and no id repeats; a body over a cap gets that cap's error |
| `rpc_postcard_frame` | the postcard codec (`id_effect_rpc`, feature `postcard`): `decode_frame`, `decode_call`, `decode_result`, `decode_exit`, `split_record`, and `decode_limited` on a recursive type and on `Vec<()>` | frames and calls round-trip; records make progress within the 64 KiB cap; nesting stays within `MAX_DEPTH`; zero-sized elements stay within the input's element budget |
| `rpc_duplex_frame` | `transport::frame::decode`, the frame boundary shared by WebSocket and WebTransport: the `idrpc.json.v1` path, and the `idrpc.postcard.v1` path the server takes (`decode_frame` + `frame::validate`); a `resume` frame's payload through `resume::decode_resume` (`id_effect_rpc`) | a decoded frame is valid and keeps its envelope through a round trip (when the re-encoding fits the cap) |
| `rpc_resume_cursor` | `resume::decode_cursor` on text and `resume::decode_resume` on a JSON payload (`id_effect_rpc`) | a cursor is a sequence cursor of at most 64 bytes with one spelling; an accepted resume has at most 1,024 distinct call ids, none 0, each with a sequence cursor |
| `sse_last_event_id` | `LastEventId::parse` (`id_effect_axum`, feature `sse`), then `wire::Cursor::decode` as the RPC SSE transport does | an accepted id is at most 256 bytes of UTF-8 without CR, LF or NUL and unchanged; a cursor from it re-spells to it |
| `i18n_accept_language` | `negotiate::parse_accept_language`, then `Negotiator::select` with the input as header, cookie and preference (`id_effect_i18n`, 0.13) | a value over 256 bytes is refused whole; an accepted one has at most 32 ranges, best first, weights in `1..=1000`, locales that re-parse to themselves; the chosen locale is always allowlisted |
| `sql_page_cursor` | `repository::page::PageCursor::decode` on the token and on the base64url of the input (`id_effect_sql`, feature `repository`, 0.13) | a token over 1 KiB is refused before decoding; a cursor has 1 to 8 keys, none NULL or JSON, and round-trips through `encode` and `PageCursor::new` |
| `mcp_jsonrpc_message` | `mcp::decode_message` and `decode_message_with_limit` (`id_effect_ai`, feature `mcp`, 0.13) | a message over the cap is refused unparsed; an accepted one has a 1 to 256-byte method, a bounded string id and object `params`, and re-encodes to a message of the same kind, method, id and error; refusals map to the parse or invalid-request error |
| `ai_sse_event` | `stream::decode_sse_event` and the incremental `SseParser` (`id_effect_ai`, 0.13) | input over 1 MiB is refused; an event has a field and no line break inside one; feeding the bytes in two chunks gives the same first event |
| `ai_tool_arguments` | `tools::decode_arguments`, then `ToolSet::invoke` under the default `ToolPolicy` (`id_effect_ai`, feature `tools`, 0.13) | decoded arguments meet the type's refinements and round-trip through its `Schema`; `invoke` runs a handler exactly when the arguments decode for a registered read tool; unregistered names and write tools run nothing |
| `admin_form_body` | `forms::decode_form`, then the mass-assignment guard `FormBody::bind` (`id_effect_admin`, 0.13) | a body over 64 KiB is a 413, others decoded have at most 1,024 pairs that round-trip through re-encoding; `bind` keeps exactly the declared fields and reports every other name once, never the CSRF field or `_`-prefixed names |

Some inputs need a format the fuzzer can reach:

- `config_profile_toml`, `jwt_decode`: a `\n---\n` line splits the input into two documents (the
  two TOML files; the JWT header and claims, which the target base64url-encodes and signs with
  the verifier's HS256 secret, so the claims checks see fuzzed JSON behind a valid signature).
- `route_query`, `route_form`: the first byte picks the content type and the schema.
- `rpc_postcard_frame`: the first byte picks the decoder (byte % 7, in the order of the table
  row); `rpc_duplex_frame`: the first byte picks the codec (even JSON, odd postcard);
  `rpc_batch`: the first two bytes pick the call cap (`1..=64`) and the byte cap (× 256).
- `mcp_jsonrpc_message`: the first byte also picks a small cap (× 16 bytes) for a second decode;
  `ai_sse_event`: the first byte picks where the bytes are split into two chunks;
  `ai_tool_arguments`: the first byte picks the tool name (byte % 5: `lookup`, `discount`, the
  write tool `wipe`, the unregistered `rm_rf`, the case variant `Lookup`), the rest is the JSON
  arguments.
- `sql_page_cursor` decodes the input both as the token and base64url-encoded, so its seeds are
  plain cursor documents (`{"v":1,"f":…,"k":[…]}`) as well as one token.

The 0.13 seeds (`fuzz/corpus/{i18n_accept_language,sql_page_cursor,mcp_jsonrpc_message,
ai_sse_event,ai_tool_arguments,admin_form_body}`) cover, besides valid inputs, the bounds: an
`Accept-Language` of 33 ranges and one over 256 bytes, a cursor of 9 keys and one with a NaN, an
out-of-range `i16` and a JSON key, an MCP batch and a response carrying `error` too, SSE with CR LF
and comment lines, a refined-out tool argument, a write tool and an unregistered tool, and a form
that submits the primary key, the tenant column and invalid escapes.

The 0.12 seeds (`fuzz/corpus/rpc_*`, `sse_last_event_id`) include, besides valid inputs, the
shapes the bounds exist for: a 500-deep recursive value and a sequence of 2^40 empty elements
(`rpc_postcard_frame`), a record prefix over the cap, a 2,000-element batch of empty arrays,
duplicate and zero call ids in a resume, a padded cursor, and a `Last-Event-ID` with CR LF.

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
  them for 5 minutes each (`fuzz-seconds: 6900` for the 23 targets; it is split across the
  targets, so raise it by 300 for each target you add, and the job's `timeout-minutes` with it),
  starting from the stored corpus. A crash the base branch does not have fails the job.
- **`.github/workflows/cflite_batch.yml`** runs nightly (and on demand): 10 minutes per target
  (`fuzz-seconds: 13800`; raise it by 600 per added target, and the job's `timeout-minutes` with
  it), then corpus pruning. The corpus is stored as workflow artifacts, which the next batch and the
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
