//! Book part 6: one `#[cfg(doctest)]` include per chapter file.

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part6/ch26-00-platform-introduction.md")]
struct Ch2600PlatformIntroduction;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part6/ch27-00-observability.md")]
struct Ch2700Observability;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part6/ch27-01-metrics.md")]
struct Ch2701Metrics;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part6/ch27-02-health.md")]
struct Ch2702Health;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part6/ch28-00-data.md")]
struct Ch2800Data;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part6/ch29-00-api-boundaries.md")]
struct Ch2900ApiBoundaries;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part6/ch30-00-application.md")]
struct Ch3000Application;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part6/ch31-00-async-messaging.md")]
struct Ch3100AsyncMessaging;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part6/ch32-00-workflow-cluster.md")]
struct Ch3200WorkflowCluster;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part6/ch35-00-ai.md")]
struct Ch3500Ai;
