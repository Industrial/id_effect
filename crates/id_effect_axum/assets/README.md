# Vendored assets of `id_effect_axum`

Files in this directory are shipped inside the crate and served by the app itself (same origin,
no CDN). Each one is pinned by its hash; `tests/ui.rs` (`htmx_asset_hash_matches_readme`) checks
the file against the hash below, and `id_effect_axum::ui::htmx::HTMX_INTEGRITY` carries the same
value into the `<script integrity>` attribute. Formatters skip these files (`treefmt.toml`,
`treefmt.ci.toml`): they are kept byte-for-byte.

## htmx

| | |
|---|---|
| File | `htmx-2.0.11.min.js` |
| Version | htmx 2.0.11 (`dist/htmx.min.js` of the npm package `htmx.org@2.0.11`) |
| Source | <https://registry.npmjs.org/htmx.org/-/htmx.org-2.0.11.tgz> |
| Tarball integrity (npm) | `sha512-Thx/WtpeOQqSrqBCw/A1cwGJGg4UrVa3+sW0GmrM3p4gJgO89ecH4qtbnyzDDWFvBTqjnIMCgELTNt636dtamA==` |
| File integrity (SRI) | `sha384-2OatzQy1H+Zd/IIrjr1TcuDGqLXeHhbooAyJY1KdQMKnr4LZ22k31GBLdYKHmVjg` |
| Size | 52182 bytes |
| License | 0BSD (Zero-Clause BSD), text below |
| Served at | `/assets/htmx-2.0.11.min.js` by `ui::htmx::asset_route()` |

The page side is configured by `ui::htmx::htmx_head` (used by `ui::BasicLayout`) through
`<meta name="htmx-config">`: `allowEval: false`, `allowScriptTags: false`,
`selfRequestsOnly: true`, `includeIndicatorStyles: false` (no inline `<style>`, so a strict
`style-src` holds), `historyCacheSize: 0` (no page snapshots in `localStorage`). The pages are
served with a Content-Security-Policy whose `script-src` is `'self'` (plus the per-request nonce of
`SecurityLayer::recommended`), so the vendored file is the only script source needed.

### Updating

An update is its own reviewed change (Risk 10 of the 0.13 release plan):

1. Download the tarball of the new 2.0.x version from the npm registry and check its `sha512`
   against `npm view htmx.org@<version> dist.integrity`.
2. Copy `package/dist/htmx.min.js` to `htmx-<version>.min.js` here and delete the old file.
3. Compute `openssl dgst -sha384 -binary htmx-<version>.min.js | base64 -w0`, update the table
   above, `HTMX_VERSION` and `HTMX_INTEGRITY` in `src/ui/htmx.rs`.
4. Read the htmx changelog for changes to `htmx.config` defaults, then run
   `cargo nextest run -p id_effect_axum --features ui`.

### License (0BSD)

```text
Zero-Clause BSD
=============

Permission to use, copy, modify, and/or distribute this software for
any purpose with or without fee is hereby granted.

THE SOFTWARE IS PROVIDED “AS IS” AND THE AUTHOR DISCLAIMS ALL
WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES
OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE
FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY
DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN
AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT
OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```
