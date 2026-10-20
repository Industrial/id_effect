---
title: 'Follow-up — TypeScript client resumes from the last item the caller consumed: r12-f-ts-cursor'
slug: r12-f-ts-cursor
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F58: `RpcStream.cursor` (and every resume the runtime sends: WS/WT `resume`, SSE `Last-Event-ID`, subscription re-`call`) is the cursor of the last item the caller has CONSUMED from the iterator, not the last item received; buffered-but-unread items are re-delivered after a resume, never skipped.'
  - 'Credits stay consistent with the design note: an item is credited when consumed; after a resume the server counts items up to the sent cursor as credited (handoff-resume-012.md).'
  - 'Deno test `resume_does_not_skip_buffered_unread_items` (server sends N items, client reads k < N, connection drops, client resumes → receives items k+1..N exactly once, in order) for a server stream and a subscription; goldens regenerated (`UPDATE_GOLDEN=1`).'
  - 'Gate: `ID_EFFECT_REQUIRE_DENO=1 cargo nextest run -p id_effect_tool --test rpc_gen_ts`; `deno test` / `deno check` under crates/id_effect_tool/tests/ts; `id-effect rpc check --generated` for Ledger and Relay web clients regenerated if their checked-in output changes.'
  - 'Owns: `crates/id_effect_tool/src/rpc/ts/**`, `crates/id_effect_tool/tests/{rpc_gen_ts.rs,golden/rpc/**,ts/**}`, regenerated client output under `examples/reference/{ledger,relay}/**` only where generated, CHANGELOG own hunk.'
---

# r12-f-ts-cursor

Coordinator follow-up leaf of `.maestro/specs/releases/0.12.0.md` (E12.4), from F58 (reported by r12-w7-relay).
