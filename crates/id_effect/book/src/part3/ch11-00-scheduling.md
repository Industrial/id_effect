# Scheduling — Retry, Repeat, and Time

Production services fail. Networks are unreliable. Downstream APIs go down. The database gets overwhelmed. Defensive engineering means anticipating failure and building policies for what to do when it happens.

id_effect models these policies with `Schedule<Out, In>` — an immutable value that describes when to recur, how long to wait in between, and when to give up — applied with `Effect::retry` and `Effect::repeat`. Every delay is slept on the fiber's **executor clock**, so under the deterministic `TestExecutor` scheduling logic is tested in virtual time, without real delays.

**Temporal scheduling** (this chapter) is separate from [**Compute Fabric**](./ch12-00-compute-fabric.md) **admission**: `Schedule` governs *when to retry*; the fabric governs *how many* parallel children run at once against CPU and memory policy. Both can apply in the same program — e.g. retry a remote step while the fabric throttles local fiber admission.
