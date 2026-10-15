//! Racing and timeouts on fibers: `race`, `race_all` and `timeout`. Empty until
//! `c-race-timeout` fills it.
//!
//! The first success wins; the losers are interrupted and their finalizers have run before the
//! result is returned, and two failures combine into `Cause::Both`. `timeout` races the effect
//! against the executor clock (virtual time on `TestExecutor`); a masked child delays it.
