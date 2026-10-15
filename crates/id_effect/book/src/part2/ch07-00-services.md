# Services — The Complete DI Pattern

The previous chapters established the building blocks: capabilities (plain types in the typed
environment `R`), `caps!` / `HCons` lists, and `Layer`s (recipes that build capabilities). Now we
put them together into the complete *service* pattern.

A service in id_effect is the combination of:

1. A **trait** defining the interface (methods return `Effect`s).
2. A **capability type** that holds an implementation in `R` — usually a newtype over
   `Arc<dyn Trait>`.
3. One or more **implementations** (production and test).
4. A **`Layer`** that builds the capability, possibly from other capabilities.

Requirements are part of the effect's type (`caps!(UserRepo, Email)`); a program whose `R` is not
fully provided does not compile. By the end of this chapter you'll have a multi-service program
wired at the edge with layers and tested with plain values.

The sections [Tokio bridge](./ch07-05-tokio-bridge.md) through [Logging](./ch07-11-logger.md)
apply the same pattern to the **workspace integration crates**: running effects on Tokio,
platform I/O and HTTP, hosting handlers under **Axum** / **Tower**, and injecting
**configuration**, **logging** and **OpenTelemetry**. Read them in order when wiring a
production binary.
