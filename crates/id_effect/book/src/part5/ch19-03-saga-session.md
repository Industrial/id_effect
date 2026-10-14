# Sagas

`Saga` runs forward `Effect` steps in order. `Saga::run` returns one `Effect<(), SagaError<E>, R>`: when a forward step fails, the completed steps' compensations run in **reverse** order and the effect fails with `SagaError::Forward(e)`; a failing compensation aborts the rollback with `SagaError::Compensate(e)`.

```rust
use std::sync::{Arc, Mutex};
use id_effect::{Effect, fail, run_blocking};
use id_effect_fsm::{Saga, SagaError, SagaStep};

// The environment is an audit log shared by every step.
type Audit = Arc<Mutex<Vec<&'static str>>>;

fn record(entry: &'static str) -> Effect<(), String, Audit> {
    Effect::new(move |log: &mut Audit| {
        log.lock().unwrap().push(entry);
        Ok(())
    })
}

let saga = Saga::new()
    .step(SagaStep::with_compensate("reserve", || record("reserve"), || record("release")))
    .step(SagaStep::forward("charge", || fail("card declined".to_string())));

let audit = Audit::default();
let err = run_blocking(saga.run(), Arc::clone(&audit)).unwrap_err();
assert_eq!(err, SagaError::Forward("card declined".to_string()));
assert_eq!(*audit.lock().unwrap(), ["reserve", "release"]);
```

A saga holds its steps behind `Arc`s, so `run` can be called repeatedly; each call builds a fresh effect.

The linear session-type markers (`SessionSend` / `SessionRecv`) that earlier releases shipped here were removed in 0.6 (ADR 0010: off-vision for the runtime tier).
