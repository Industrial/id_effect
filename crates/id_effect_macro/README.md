# id_effect_macro

Declarative (`macro_rules!`) macros for [`id_effect`](https://github.com/Industrial/id_effect).
You normally do not depend on this crate directly: `id_effect` re-exports every macro, and the
expansions refer to `::id_effect::…` paths. The procedural `effect!` macro and the derives live in
`id_effect_proc_macro`.

| Macro | Expands to |
|-------|------------|
| `pipe!(x, f, g)` | `g(f(x))` — left-to-right application |
| `err!(A \| B \| C)` | `Or<A, Or<B, C>>` — type-level error union (`err!()` is `()`) |

### Removed in 0.7: the v1 capability-DI macros

`caps!`, `provide!`, `providers!`, `require!` and `mock_capability!` expanded to the 0.6 runtime
capability types (`Env`, `CapList`, `ProviderBox`, `ProviderSpec`) and were removed with the v1
kernel (k-retire-v1). Their 0.7 replacements:

| 0.6 macro | 0.7 replacement |
|-----------|-----------------|
| `caps!(A, B)` (`CapList<(Cap<A>, Cap<B>)>`) | `id_effect::caps!(A, B)`, now the proc macro from `id_effect_proc_macro` (`HCons<A, HCons<B, HNil>>`) |
| `provide!(P)` | a `Layer` (`P::layer()`, `Layer::succeed(hlist![..])`) |
| `providers!(name: [P, Q])` | `P::layer().merge(Q::layer())` |
| `mock_capability!(M, Svc, "id", \|\| v)` | `.provide(v)` / `.provide_env(hlist![v])` / `Layer::succeed(hlist![v])` |
| `require!(Key)` | `~Key` inside `effect!`, `need::<Key>()` elsewhere |

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
[`tests/macros.rs`](tests/macros.rs).
