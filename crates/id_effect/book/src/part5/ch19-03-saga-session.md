# Sagas

`Saga` runs forward `Effect` steps in order. `Saga::run` returns one `Effect<(), SagaError<E>, R>`: when a forward step fails, the completed steps' compensations run in **reverse** order and the effect fails with `SagaError::Forward(e)`; a failing compensation aborts the rollback with `SagaError::Compensate(e)`. Only typed failures are compensated; a defect or an interrupt passes through untouched.

Each step is a pair of effect **factories** (`Fn() -> Effect`), because effects are one-shot:

```rust
use std::sync::{Arc, Mutex};
use id_effect::{Cause, Effect, Exit, caps, fail, need};
use id_effect_fsm::{Saga, SagaError, SagaStep};

// The environment carries an audit log shared by every step.
#[derive(Clone, Default)]
struct Audit(Arc<Mutex<Vec<&'static str>>>);

fn record(entry: &'static str) -> Effect<(), String, caps!(Audit)> {
    need::<Audit>().map(move |audit| audit.0.lock().unwrap().push(entry))
}

let saga = Saga::new()
    .step(SagaStep::with_compensate("reserve", || record("reserve"), || record("release")))
    .step(SagaStep::forward("charge", || fail("card declined".to_string())));

let audit = Audit::default();
let exit = saga.run().provide(audit.clone()).block_on();
assert!(matches!(
    exit,
    Exit::Failure(Cause::Fail(SagaError::Forward(ref e))) if e == "card declined"
));
assert_eq!(*audit.0.lock().unwrap(), ["reserve", "release"]);
```

A saga holds its steps behind `Arc`s, so `run` can be called repeatedly; each call builds a fresh effect. Because the result is an ordinary `Effect`, a saga composes with retries, timeouts and other effects instead of blocking.

The linear session-type markers (`SessionSend` / `SessionRecv`) that earlier releases shipped here were removed in 0.6 (ADR 0010: off-vision for the runtime tier).
