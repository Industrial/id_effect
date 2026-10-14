# `Cause::Die(String)` → `Cause::Die(Defect)`

`Cause::Die` now carries a `Defect`: the message plus, for panics, the original payload
(`Defect::from_panic`, `Cause::from_panic`). `Cause::die` / `Exit::die` still accept `&str`
and `String`. `Eq`/`Hash` compare the message only.

```rust
// 0.6
Cause::Die(msg) => eprintln!("{msg}"),          // msg: String
// 0.7
Cause::Die(d) => eprintln!("{}", d.message()),  // d: Defect (also Display)
let p = d.take_payload();                       // Option<Box<dyn Any + Send>>
d.with_payload(|e: &MyPanic| e.code);           // downcast in place
```
