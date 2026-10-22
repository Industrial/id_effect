# Recipe: Leptos SSR + hydration on the id_effect host

[Leptos](https://leptos.dev) renders the page on the server and hydrates it in the browser; the
id_effect axum host serves it through typed routes behind the secure stack; the page's data comes
from an id_effect RPC service through the Rust client `#[rpc::service]` generates.

This is a **recipe**, not an integration crate (ADR 0015 D4): it lives outside the workspace
(the root `Cargo.toml` excludes `examples/recipes`), has its own `Cargo.lock`, and is built and
tested by the CI job `recipe-leptos`. Book chapter: *Leptos recipe* (part 6,
`crates/id_effect/book/src/part6/ch29-11-leptos-recipe.md`).

| Piece | Where |
|---|---|
| The UI (both builds) | `src/app.rs`: `shell`, `App`, `NoteList`, `Counter` |
| The RPC service, its guard and store | `src/rpc.rs`: `#[rpc::service] trait Notes`, `ServiceToken`, `NoteStore` |
| The host | `src/server.rs`: `routes`, `app`, `NotesApi` (the generated `NotesClient` over HTTP) |
| The browser entry point | `src/lib.rs`: `hydrate()` (feature `hydrate`) |
| Tests | `tests/recipe.rs` |

## Pins

| What | Version |
|---|---|
| `leptos` | `=0.8.22` (features `ssr` + `nonce` on the server, `hydrate` in the browser) |
| `leptos_axum` | `=0.8.10` (axum 0.8, the axum id_effect_axum uses) |
| `any_spawner` | `=0.3.0` (Leptos's executor, set to tokio) |
| `wasm-bindgen` / `wasm-bindgen-cli` | `=0.2.129` (must be equal) |
| `console_error_panic_hook` | `=0.1.7` |
| Rust | the repository's pinned toolchain (`rust-toolchain.toml`), plus the `wasm32-unknown-unknown` target |

## Build: no npm

The browser half is this same crate compiled for **`wasm32-unknown-unknown`** with feature
`hydrate`, bound by **`wasm-bindgen-cli`** (`--target web`) into `target/site/pkg`. There is no
npm, no `node_modules`, no bundler and no `cargo-leptos`: `cargo` and `wasm-bindgen` are the whole
tool chain. `scripts/build-site.sh` runs both steps and checks that the installed
`wasm-bindgen-cli` matches `Cargo.lock`.

```bash
cd examples/recipes/leptos-ssr
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
scripts/build-site.sh            # → target/site/pkg/leptos_ssr.js + leptos_ssr_bg.wasm
cargo run                        # http://localhost:3000
```

On a nightly toolchain without the target's prebuilt std (a Nix toolchain, for instance),
`BUILD_STD=1 scripts/build-site.sh` builds std from `rust-src` (`-Zbuild-std`).

Environment of the server: `LEPTOS_SSR_ADDR` (default `127.0.0.1:3000`), `LEPTOS_SSR_SITE_ROOT`
(default `target/site`), `NOTES_SERVICE_TOKEN` (default: a random token per start). Open
`http://localhost:3000`, not the IP address: the CSRF cookie is `__Host-` and `Secure`, which
browsers accept on `localhost` only.

## Test

```bash
cargo nextest run --manifest-path examples/recipes/leptos-ssr/Cargo.toml
```

- `leptos_recipe_ssr_renders` — the document Leptos renders on the host: notes from the RPC
  service, untrusted text escaped, the hydration bundle referenced, the resolved resource
  serialized into the page, a nonce CSP whose nonce is on every inline script, the security
  headers and CSRF cookie; the bundle and stylesheet public, everything else `404`; every route
  and procedure with an access decision and no `dangerous_*` opt-out in the process.
- `leptos_recipe_calls_rpc` — the generated `NotesClient` over HTTP: `list`, `add`, the typed
  `NoteError`; the page shows what the service holds; a wrong token is `401` (the guard) and no
  token `403` (CSRF: only bearer calls skip it), both before any procedure runs; a
  cookie-carrying call without a CSRF token is `403` even with the right token; a renderer with
  the wrong token renders "unavailable" and no data (fail closed).
- `leptos_recipe_serves_built_bundle` — the real bundle from `scripts/build-site.sh`, served with
  its media types. Skipped when absent, unless `LEPTOS_SSR_REQUIRE_SITE=1` (CI).

## Secure by default

- **Typed routes, deny by default.** `GET /`, `GET /pkg/{*file}` and `GET /recipe.css` are
  `.public()` endpoints; `POST /rpc` carries the `Notes` procedures, each `#[guard(ServiceToken)]`.
  Any other path is a `404` from the typed router.
- **The 0.11 security stack.** `SecurityLayer::recommended`: CSRF on every unsafe method, the
  security headers, and a nonce CSP. The handler passes the request's `CspNonce` to Leptos
  (`leptos::nonce::Nonce::from_value`), so the hydration script and the resource scripts carry
  it. The only addition to `ContentSecurityPolicy::recommended()` is `'wasm-unsafe-eval'`, which
  browsers require to compile WebAssembly under a CSP; it does not allow JavaScript `eval`.
- **Service token, server side only.** The renderer calls `/rpc` with `Authorization: Bearer`
  and no cookie; `csrf_exempt_bearer()` lets exactly such calls skip the CSRF check (a browser
  cannot send them cross-site on its own). The browser never sees the token and makes no RPC
  call: the data arrives serialized in the page.
- **SSRF guard kept.** The platform HTTP client still refuses private and loopback addresses;
  only the RPC host is allowlisted (`HttpClientConfig::allow_hosts`).
- **No `dangerous_*` opt-out** is taken; the test asserts the posture registry is empty.

## Where to go from here

- Interactive calls from the browser: a Leptos server function, or a browser RPC client with the
  CSRF header — a session-authenticated route, not the service token.
- Several pages: `leptos_router` with one typed `.public()` (or `.guard(..)`) endpoint per route.
- Production: `PROFILE=release`, `wasm-opt -Oz` on `leptos_ssr_bg.wasm` (binaryen, optional),
  the cookie key from config (`CookieKey::layer`) instead of `CookieKey::generate()`.
