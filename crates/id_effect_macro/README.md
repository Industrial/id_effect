# id_effect_macro

Declarative (`macro_rules!`) macros for [`id_effect`](https://github.com/Industrial/id_effect).
You normally do not depend on this crate directly: `id_effect` re-exports every macro, and the
expansions refer to `::id_effect::…` paths. The procedural `effect!` macro and the derives live in
`id_effect_proc_macro`.

| Macro | Expands to |
|-------|------------|
| `pipe!(x, f, g)` | `g(f(x))` — left-to-right application |
| `err!(A \| B \| C)` | `Or<A, Or<B, C>>` — type-level error union (`err!()` is `()`) |
| `caps!(A, B)` | `CapList<(Cap<A>, Cap<B>)>` — required capability set (`caps!()` is `Env`) |
| `provide!(P)` | `ProviderBox::new::<P>()` |
| `providers!(name: [P, Q])` | `[ProviderBox; 2]` — a named provider bundle |
| `mock_capability!(M, Svc, "id", \|\| value)` | a `ProviderSpec` struct `M` that supplies `Svc` |
| `require!(Key)` | only valid inside `effect!` (a `compile_error!` elsewhere) |

## Quick start

```rust
use id_effect::{Cap, Or, build_env, err, mock_capability, pipe, providers};

#[derive(Clone)]
struct Greeting(&'static str);

mock_capability!(GreetingMock, Greeting, "greeting/mock", || Greeting("hello"));

struct ParseErr;
struct RangeErr;

fn parse_percent(s: &str) -> Result<u8, err!(ParseErr | RangeErr)> {
  let n: u32 = s.trim().parse().map_err(|_| Or::Left(ParseErr))?;
  u8::try_from(n).ok().filter(|n| *n <= 100).ok_or(Or::Right(RangeErr))
}

assert_eq!(pipe!(" 7 ", str::trim, str::len), 1);
assert!(parse_percent("42").is_ok());

let env = build_env(providers!(dev: [GreetingMock])).unwrap();
assert_eq!(env.get::<Cap<Greeting>>().0, "hello");
```

See [`examples/macros_tour.rs`](examples/macros_tour.rs) and the expansion tests in
[`tests/macros.rs`](tests/macros.rs).
