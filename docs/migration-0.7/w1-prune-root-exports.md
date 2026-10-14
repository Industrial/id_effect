# Root module aliases and unused helpers removed

`id_effect::{channel, ref_, either, func, clock, duration, schedule, data, stream, snapshot}`
aliases are gone; use the real module path (or the root item, e.g. `id_effect::Duration`).
`pipe1`/`pipe2`/`pipe3`, `flip`, `const_`, `tupled`, `untupled`, `memoize` and `MutableRef` are
removed from core (`identity`, `always`, `compose` stay).

```rust
// 0.6                                  // 0.7
use id_effect::channel::QueueChannel;   use id_effect::coordination::channel::QueueChannel;
use id_effect::duration::Duration;      use id_effect::scheduling::duration::Duration;
use id_effect::data::EffectData;        use id_effect::schema::data::EffectData;
use id_effect::stream::StreamSender;    use id_effect::streaming::stream::StreamSender;
use id_effect::{pipe2, flip};           use id_effect_fp::foundation::function::{pipe2, flip};
use id_effect::MutableRef;              // use std::sync::Mutex / FiberRef / SynchronizedRef
```
