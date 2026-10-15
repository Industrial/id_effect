//! The `Random` capability: `next_u64`, `next_f64` in `[0, 1)`, `next_range` and `shuffle`, on
//! an internal xoshiro256** generator (no new dependency). Empty until `p-random` fills it; the
//! deterministic `TestRandom` (`seeded`, `feed`) is exported from
//! [`testing`](crate::next::testing).
