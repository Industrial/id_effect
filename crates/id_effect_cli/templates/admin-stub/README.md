# Admin scaffold stub

Placeholder internal admin UI — wire routes into your Axum host.
Not a production admin product; extend with your auth layer.

## Why this directory stays

Nothing generates, builds or tests it: `id-effect new` uses its own templates in
`crates/id_effect_tool/templates/`. It is kept because the capability scorecard cites it as the
evidence of its admin row (`docs/scorecard/scorecard.json`, "Template stub only"). Remove it
together with that evidence when the scorecard is next re-scored.
