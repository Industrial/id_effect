# Scheduling — Retry, Repeat, and Time

Production services fail. Networks are unreliable. Downstream APIs go down. The database gets overwhelmed. Defensive engineering means anticipating failure and building policies for what to do when it happens.

id_effect models these policies with `Schedule` — a type that describes when to retry, how long to wait between attempts, and when to give up. Combined with `Clock` injection, scheduling logic becomes testable without real-time delays.

**Temporal scheduling** (this chapter) is separate from [**Compute Fabric**](./ch12-00-compute-fabric.md) **compute scheduling**: `Schedule` governs *when to retry*; Fabric governs *where and how concurrently* effects run against CPU and memory policy. Both can apply in the same program — e.g. retry a remote step while the supervisor throttles local fiber admission.
