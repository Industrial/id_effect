# Capabilities and the Typed Environment

Chapter 4 showed how `R` encodes dependencies. This chapter looks at what goes into `R` and how
effects read it:

- why positional `R` (tuples) breaks down, and why lookup by type does not;
- capability types: plain values, newtypes and `Arc<dyn Trait>` services;
- the environment value: an HList of `Arc`ed capabilities (`HCons` / `HNil`, `hlist!`), and the
  runtime-checked `DynEnv` for plugin-style wiring;
- reading capabilities: `~Cap` in `effect!`, `need::<Cap>()` outside it.

By the end you'll know why the order of capabilities in `R` never matters to the code that reads
them, and why a missing one is a compile error.
