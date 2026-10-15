# Compute Fabric admission: `Fabric` and `Concurrency::Inherit`

In 0.6 the Compute Fabric admitted work by blocking a thread (`AdmissionController::acquire_blocking`,
used by `ComputeFabric::spawn_admitted`). In 0.7 the fiber runtime admits asynchronously, and only
for `Concurrency::Inherit` (Effect.ts `concurrency: "inherit"`).

```rust
// 0.7
let fabric = Fabric::with_capacity(2);                     // or Fabric::from_compute(Arc::clone(&compute_fabric))
fabric.provide(all(effects, Concurrency::Inherit))         // at most 2 children run at once
fabric.provide(for_each_par(ids, fetch, Concurrency::Inherit))

all(effects, Concurrency::Inherit)                         // no ambient fabric: Unbounded
fabric.provide(all(effects, Concurrency::Bounded(8)))      // local window of 8; fabric not consulted
```

- The ambient fabric is fiber-local (like Effect.ts `Effect.withConcurrency`): `Fabric::provide`
  sets it for an effect and the children it forks; `Fabric::current()` reads it. It is not a
  capability in `R`, and the process-wide `id_effect::compute::install_fabric` is not consulted.
- Each `Inherit` child holds one permit while it runs and gives it back when it exits, however
  it exits. A throttled child is a suspended future (no thread is held); interrupting the waiting
  fiber cancels the wait without taking a permit.
- Nested `Inherit` forks do not deadlock: a fiber that holds a permit lends it to one of its own
  `Inherit` children at a time.
- `AdmissionController::admit()` / `admit_n(n)` and `ComputeFabric::admit()` are the new async
  admissions (an `Admit` future resolving to an `AdmissionGuard`, which releases on drop). A
  custom admission implements `next::Admission` and is wrapped with `Fabric::new`.
