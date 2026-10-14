# Spike corpus

The shared yardstick for the two 0.7 kernel spikes (`k-spike-a`, `k-spike-b`). It has two parts:

- [`handlers.md`](handlers.md): 20 handler shapes (`H01`–`H20`) and one edge-wiring shape (`W01`),
  each spelled behaviourally so that neither design is favoured.
- `spike_corpus` (this crate, `src/lib.rs`): the services, errors and `Req` type every shape uses.
  It has no dependencies and no executor. Its async methods return `Send` futures that suspend
  once, and `Db::hang` never completes, for H20.

Each spike implements every shape in `spikes/kernel_{a,b}/tests/corpus.rs`, with `spike_corpus` as a
dev-dependency (already wired). It can be split across several files under `tests/` if it gets long;
pass them all to the counter.

## Marking up an implementation

```rust
// corpus:begin H01
fn hello() -> Effect<String, Never, caps!(Greeting)> {
  need::<Greeting>().map(|g| g.0.to_string())
}

#[test]
fn h01_single_capability_read() {
  let out = hello().provide(Greeting("hello from effect-axum")).block_on();
  assert_eq!(out.unwrap(), "hello from effect-axum");
}
// corpus:end H01

// corpus:unsupported H10 no generic R ∪ T; needs <what>, see <file>
```

- A region holds the handler, everything the shape's **Region** line lists, and the `#[test]` that
  asserts the shape's **Do** line. Every shape must have that test.
- Regions do not nest, and each id appears exactly once across all files.
- Use `corpus:unsupported <ID> <reason>` only when the design genuinely cannot express the shape. Do
  not use it for "not finished yet". The gate treats an unsupported shape as a falsifier hit for that
  design. A shape that is neither implemented nor declared unsupported is reported as `missing`.

## What counts as an annotation

`spikes/scripts/count-annotations.sh` counts, inside regions, after removing comments and string
literals:

| Column | Counts | Example |
|---|---|---|
| `turbofish` | every `::<` | `need::<Db>()`, `.narrow::<caps!(Db), _>()`, `Effect::<_, _, R>::succeed` |
| `let_ty` | `let` bindings with a type ascription | `let e: Effect<u32, Never, R> = ...`, `let (a, b): (A, B) = ...` |
| `closure_ty` | closures with at least one annotated parameter | `\|r: &mut R\| ...` |
| `marker` | lines tagged `// ann: <why>` | an annotation the patterns cannot see (below) |
| **`total`** | the sum of the four above | |
| `index` | Design A index witnesses written out (`Here`, `There<..>`) | informational, not in `total` |
| `fwd` | entries in `forward(...)` lists, inside and outside regions | B's falsifier (R1), informational |

Not counted: `fn` signatures, including generics, bounds, `where` clauses and return types, up to the
opening `{`. Both designs need a signature, so it is a declaration rather than an inference aid. Type,
struct, trait and `impl` declarations are not counted either.

The summary row reports `annotated_pct`: the share of *ported* shapes with `total > 0`. Design A's
R3(b) falsifier is above 20 % of handlers needing annotations.

Tag a line with `// ann:` when it exists only to pin inference but has none of the counted syntax.
Examples: a call to a helper that exists only to fix `R` (`pin_env(&eff)`), a `PhantomData` field
added to fix `R`, or an `as` cast that selects the env.

## Fairness rules

1. **Same services.** Use the `spike_corpus` types as they are. If the design needs a newtype or wrapper
   (B's `Service` impls, a `Cap<T>` key), it is declared in the region that needs it, or once outside
   all regions if every shape needs it. The trait impls themselves are declarations and are not
   counted.
2. **No corpus-only sugar.** A region may only use the design's proposed public API: the kernel crate
   plus its macros. Any helper or macro written for the corpus goes **inside** the region that uses
   it, so its annotations count.
3. **Idiomatic, minimal code.** Write what the design's docs would recommend to a user. If the code
   compiles without an annotation, remove the annotation. Do not contort code to dodge one either. For
   example, do not split an expression into helper fns just to gain an expected type that a user would
   not write.
4. **Probes are the point.** Each shape's **Probes** line names the risk the shape exists to expose.
   Implement the shape as specified even when the design struggles. Struggling there is the
   measurement.
5. **No `#[allow]` games.** The corpus must build warning-free under the spike workspace lints.

## Running

```bash
cd spikes
devenv shell -- cargo nextest run -p kernel_a --test corpus     # behaviour
devenv shell -- bash scripts/count-annotations.sh kernel_a/tests/corpus.rs
devenv shell -- bash scripts/count-annotations.sh --json kernel_b/tests/corpus.rs
```
