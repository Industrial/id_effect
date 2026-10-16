# R as Documentation — Self-Describing Functions

`R` is "the environment type", but in practice it is also *living documentation* that the
compiler enforces.

## The Signature Tells the Story

```rust
# use id_effect::{Effect, caps, effect};
# struct Order; struct Receipt; struct Error; struct OrderError;
# struct Database; struct PaymentGateway; struct Mailer; struct AuditLog;
# mod a { use super::*;
// Version A: traditional async. What does it use? Read the body to find out.
async fn process_order(order: Order) -> Result<Receipt, Error> {
  // 200 lines
#   Ok(Receipt)
}
# }
# mod b { use super::*;

// Version B: effect-based. What does it use? Read the signature.
fn process_order(order: Order)
  -> Effect<Receipt, OrderError, caps!(Database, PaymentGateway, Mailer, AuditLog)>
{
  // ...
#   effect!({ let _ = (~Database, ~PaymentGateway, ~Mailer, ~AuditLog); Receipt })
}
# }
# fn main() {}
```

## Code Review Benefits

If someone adds a metrics call inside `process_order`, the body only compiles once `Metrics` is in
`R`, so the signature changes in the diff:

```diff
- fn process_order(order: Order) -> Effect<Receipt, OrderError, caps!(Database, PaymentGateway, Mailer, AuditLog)>
+ fn process_order(order: Order) -> Effect<Receipt, OrderError, caps!(Database, PaymentGateway, Mailer, AuditLog, Metrics)>
```

With parameters threaded by hand or with globals, a new dependency can appear silently in a body.

## Refactoring Safety

Remove `AuditLog` from `process_order`'s `caps!` and every `.provide(audit_log)` aimed at it stops
compiling: providing a capability the effect does not need is a type error. The compiler walks you through the
wiring clean-up. (`provide_env` and `provide_layer` accept extra capabilities, so an unused
capability in an application environment or layer graph is allowed, but it is visible there.)

## Testing Clarity

The type tells a test exactly what to supply, no more, no less:

```rust
# use id_effect::{Effect, Exit, caps, effect, hlist};
# struct Order; struct Receipt; struct OrderError;
# struct Database; struct PaymentGateway; struct Mailer; struct AuditLog;
# impl AuditLog { fn memory() -> Self { AuditLog } }
# fn mock_db() -> Database { Database }
# fn mock_payment() -> PaymentGateway { PaymentGateway }
# fn mock_mailer() -> Mailer { Mailer }
# fn test_order() -> Order { Order }
# fn process_order(_: Order) -> Effect<Receipt, OrderError, caps!(Database, PaymentGateway, Mailer, AuditLog)> {
#   effect!({ let _ = (~Database, ~PaymentGateway, ~Mailer, ~AuditLog); Receipt })
# }
// In a #[cfg(test)] module, under #[test]:
fn process_order_charges_once() {
  // R = caps!(Database, PaymentGateway, Mailer, AuditLog): four values to provide
  let exit = process_order(test_order())
    .provide_env(hlist![mock_db(), mock_payment(), mock_mailer(), AuditLog::memory()])
    .block_on();
  assert!(matches!(exit, Exit::Success(_)));
}
# process_order_charges_once();
```

Missing one is a compile error at `provide_env`, not a failure halfway through the test.

## R is Not Magic

`R` is just a type parameter. The compile-time DI comes from:

1. functions declaring what they need in `R` (usually with `caps!`);
2. capability lookups (`~Cap`, `need::<Cap>()`) checked against `R` by the type system;
3. `~` binds lifting each step into the block's environment;
4. wiring done once at the edge with `provide*` or a `Layer` graph.

No reflection, no registration, no runtime container. Just types.

The next chapter looks at the environment itself: capability types, the HList that holds them, and
how lookups work.
