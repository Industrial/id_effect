# Deprecated 0.6 aliases removed

The aliases deprecated in 0.6 are gone; use their replacements.

```rust
// 0.6 (deprecated)                         // 0.7
id_effect::StreamV1<A, E, R>                id_effect::Stream<A, E, R>
id_effect::ComputeSupervisor<E>             id_effect::FabricGovernor<E>
fabric.supervisor()                         fabric.governor()
id_effect_platform::process::CommandSpec    id_effect_platform::command::CommandSpec
```
