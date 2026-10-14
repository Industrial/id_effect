# Algebra and Type Classes

Stratum 1 in `id_effect_fp::algebra` extends Functor/Applicative/Monad with **Foldable**, **Alternative**, **Traversable**, **Bifoldable**, and **Invariant** patterns used throughout Part V crates. Since 0.6 the algebra layer lives in the maintenance-tier `id_effect_fp` crate (it was `id_effect::algebra`); add `id_effect_fp` as a dependency to use it. Examples: `cargo run -p id_effect_fp --example 090_algebra_foldable` (also `091_algebra_traversable`, `092_algebra_bifold`).
