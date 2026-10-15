# id_effect_macro

Declarative (`macro_rules!`) macros for [`id_effect`](https://github.com/Industrial/id_effect).
You normally do not depend on this crate directly: `id_effect` re-exports every macro, and the
expansions refer to `::id_effect::…` paths. The procedural `effect!` macro and the derives live in
`id_effect_proc_macro`.

| Macro | Expands to |
|-------|------------|
| `pipe!(x, f, g)` | `g(f(x))` — left-to-right application |
| `err!(A \| B \| C)` | `Or<A, Or<B, C>>` — type-level error union (`err!()` is `()`) |

### v1 capability-DI macros (removed with the v1 kernel)

These expand to the 0.6 runtime capability types (`Env`, `CapList`, `ProviderBox`,
`ProviderSpec`) and are dropped together with the v1 kernel in 0.7 (k-retire-v1). Their 0.7
replacements are on the `next` kernel:

| Macro | Expands to | 0.7 replacement |
|-------|------------|-----------------|
| `caps!(A, B)` | `CapList<(Cap<A>, Cap<B>)>` | `caps_next!(A, B)` (`HCons<A, HCons<B, HNil>>`) |
| `provide!(P)` | `ProviderBox::new::<P>()` | a `next::Layer` (`P::layer()`, `Layer::succeed(hlist![..])`) |
| `providers!(name: [P, Q])` | `[ProviderBox; 2]` | `P::layer().merge(Q::layer())` |
| `mock_capability!(M, Svc, "id", \|\| v)` | a `ProviderSpec` struct `M` | `.provide(v)` / `.provide_env(hlist![v])` / `Layer::succeed(hlist![v])` |
| `require!(Key)` | only valid inside v1 `effect!` | `~Key` inside `effect_next!`, `need::<Key>()` elsewhere |

## Quick start

```rust
use id_effect::{Or, err, pipe};

struct ParseErr;
struct RangeErr;

fn parse_percent(s: &str) -> Result<u8, err!(ParseErr | RangeErr)> {
  let n: u32 = s.trim().parse().map_err(|_| Or::Left(ParseErr))?;
  u8::try_from(n).ok().filter(|n| *n <= 100).ok_or(Or::Right(RangeErr))
}

assert_eq!(pipe!(" 7 ", str::trim, str::len), 1);
assert!(parse_percent("42").is_ok());
```

See [`examples/macros_tour.rs`](examples/macros_tour.rs) and the expansion tests in
[`tests/macros.rs`](tests/macros.rs) (the v1 DI macros: [`tests/v1_di_macros.rs`](tests/v1_di_macros.rs)).
