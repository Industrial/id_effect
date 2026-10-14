# Software Transactional Memory — Optimistic Concurrency

Shared mutable state is hard. Mutexes work but compose poorly: lock two mutexes in the wrong order and you deadlock. Lock them separately and you get torn reads. Lock the whole world and you serialise unnecessarily.

Software Transactional Memory (STM) takes a different approach: every operation on shared state runs inside a *transaction*. Transactions commit atomically or roll back and retry. No explicit locks. No deadlocks. No torn reads.

This chapter covers id_effect's STM implementation: `Stm`, `TRef`, `commit`, and the transactional collection types. Since 0.6 they live in the maintenance-tier `id_effect_fp` crate (`id_effect_fp::stm`, re-exported at the `id_effect_fp` root) rather than in core `id_effect`.
