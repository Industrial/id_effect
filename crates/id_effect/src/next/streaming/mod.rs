//! Streams on the `next` kernel: a pull-based `Stream` with no per-pull `Mutex<StreamState>`,
//! sequential `map_effect` and bounded `map_effect_par(n)`, and the 0.6 bounded-memory
//! guarantees. Empty until `m-streams` fills it.
