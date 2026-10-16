//! Book part 3: one `#[cfg(doctest)]` include per chapter file.

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch08-01-beyond-result.md")]
struct Ch0801BeyondResult;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch08-02-exit.md")]
struct Ch0802Exit;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch08-03-recovery.md")]
struct Ch0803Recovery;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch08-04-accumulation.md")]
struct Ch0804Accumulation;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch09-01-what-are-fibers.md")]
struct Ch0901WhatAreFibers;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch09-02-spawning-joining.md")]
struct Ch0902SpawningJoining;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch09-03-cancellation.md")]
struct Ch0903Cancellation;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch09-04-fiberref.md")]
struct Ch0904Fiberref;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch09-05-supervision.md")]
struct Ch0905Supervision;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch10-01-resource-problem.md")]
struct Ch1001ResourceProblem;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch10-02-scopes-finalizers.md")]
struct Ch1002ScopesFinalizers;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch10-03-acquire-release.md")]
struct Ch1003AcquireRelease;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch10-04-pools.md")]
struct Ch1004Pools;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch11-01-schedule.md")]
struct Ch1101Schedule;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch11-02-builtin-schedules.md")]
struct Ch1102BuiltinSchedules;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch11-03-retry-repeat.md")]
struct Ch1103RetryRepeat;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch11-04-clock-injection.md")]
struct Ch1104ClockInjection;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch11-05-cron.md")]
struct Ch1105Cron;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch12-00-compute-fabric.md")]
struct Ch1200ComputeFabric;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch16-00-cli-with-clap.md")]
struct Ch1600CliWithClap;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch16-02-cli-config-secret.md")]
struct Ch1602CliConfigSecret;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch16-03-config-profiles.md")]
struct Ch1603ConfigProfiles;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part3/ch16-04-secrets-vault.md")]
struct Ch1604SecretsVault;
