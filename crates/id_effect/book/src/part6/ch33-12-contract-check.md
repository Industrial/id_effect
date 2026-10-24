# Checking a contract for breaking changes

The service contract (`rpc.contract.json`, chapter 33.4) is what clients are built against: the
TypeScript client of chapter 33.11, the Rust `XClient`, and whatever a partner team generated
from the OpenAPI document. `id-effect rpc check` compares it with the contract you last
published and refuses a change that would break a client of a version that is already out.

```text
id-effect rpc check --contract crates/api/rpc.contract.json --against v0.11.0:crates/api/rpc.contract.json
```

`--against` is a file, or `<git-ref>:<path>`, read with `git cat-file` in the directory of
`--contract`. The path is from the repository root, or `./…` from the contract's directory.

The output is one line per change, with its class, where it is and why, then a count:

```text
rpc check: crates/api/rpc.contract.json against v0.11.0:crates/api/rpc.contract.json
breaking   Todos v1 add input.priority: required field added
compatible Todos v1 get output.tags: optional field added
breaking   Todos v1 get error: error variant `Gone` added (old clients cannot decode it)
compatible Todos v1 search: procedure added
4 changes: 2 breaking, 0 versioned, 2 compatible
```

Exit codes: `0` nothing breaking, `1` a breaking change (or a stale client, below), `2` a usage
error or a contract that cannot be read (either side).

## What breaks

A procedure is only ever compared with the same service *and* version. Inputs flow from old
clients, so the new server must still accept everything it accepted; outputs flow to old
clients, so the new server may only send what they can read.

| Breaking | Compatible |
|---|---|
| a procedure removed (a rename is a removal and an addition) | a procedure added |
| a version no longer served | a service or a version added |
| `kind` changed | |
| access tightened: public to guarded, a policy added | access loosened |
| a required input field added, an optional one made required | an optional input field added, a required one made optional |
| an input narrowed: a type, enum value or `anyOf` alternative removed, a bound tightened, unknown fields refused | an input widened |
| an output field removed or made optional | an output field added |
| an output widened: a type, value or alternative added, a bound loosened | an output narrowed |
| an error variant added (clients switch on `_tag` exhaustively) | an error variant removed |
| `idempotent` dropped (a retried mutation would run twice) | `idempotent` added |
| `max_items` / `max_bytes` lowered (an absent cap is the default: 10,000 items, 4 MiB) | a cap raised; `deprecated` set |

Any other schema keyword that changed (`allOf`, a `pattern` or a `format` replaced, a tuple's
length) is breaking in both directions. Descriptions never count.

## Bumping the version

A breaking change is fine in a *new* version, as long as the old one is still served (ADR 0014,
`web.rpc#6`): keep the old trait, add `#[rpc::service(version = 2, name = "Greeter")]` beside it,
mount both. `rpc check` then compares the new version with the newest older one and reports its
breaking changes as `versioned`, which pass:

```text
compatible Greeter v2: version added (compared with v1)
versioned  Greeter v2 greet input: type `string` removed (new version; v1 is still served)
compatible Greeter v2 history: procedure added
```

Dropping v1 in the same release fails with `breaking Greeter v1: version no longer served`.

When the time comes to retire v1 on purpose, say so on the command line: `--allow-retired
Greeter:1` (repeatable, one `Service:N` per retired version) lets exactly that removal pass and
prints it as `retired    Greeter v1: version no longer served (--allow-retired)`. It excuses
nothing else: any other breaking change still exits `1`, and naming a version that was not
removed (a typo, a version still served, one that never existed) is a usage error that exits `2`,
so the flag can never quietly widen the check.

## Type names

`$defs` keys are Rust `type_name` paths (`app::store::Todo`), which change whenever a module
moves. `rpc check` never compares keys: it follows each `$ref` into its own schema's `$defs` and
compares the definitions structurally, so moving a type is no change. Where a name is shown it is
the OpenAPI component name (chapter 33.10): the last `::` segment, or the shortest suffix no
other definition shares. Renaming a type (`Price` to `Cost`) is reported as compatible: the wire
shape is the same, but generated type names change.

Union alternatives are matched by their `_tag` const when they have one (error unions and tagged
enums), otherwise by their normalized text; one unmatched alternative on each side is compared
field by field.

## A stale generated client

`--generated <dir>` regenerates the TypeScript client in memory and fails when a file in `dir`
is missing or differs from what `id-effect rpc gen ts` would write:

```text
id-effect rpc check --contract rpc.contract.json --generated web/src/rpc
stale web/src/rpc/types.ts
error: the generated client is stale: run `id-effect rpc gen ts --contract rpc.contract.json --out web/src/rpc`
```

It works on its own or together with `--against`.

## In CI

Check against the last release tag on every pull request. Fetch tags first; a shallow clone has
none:

```yaml
- uses: actions/checkout@v4
  with:
    fetch-depth: 0
- run: id-effect rpc check --contract crates/api/rpc.contract.json --against "$(git describe --tags --abbrev=0):crates/api/rpc.contract.json" --generated web/src/rpc
```

The contract file itself is kept current by the snapshot test of chapter 33.4
(`assert_snapshot`, `UPDATE_GOLDEN=1`), so the check always sees what the server serves.

The same comparison is a library call, `id_effect_tool::rpc::check::compare(&previous,
&current)`, which returns the classified `Change`s.
