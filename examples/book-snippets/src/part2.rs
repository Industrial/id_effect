//! Book part 2: one `#[cfg(doctest)]` include per chapter file.

/// `part2/ch04-00-r-parameter.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch04-00-r-parameter.md")]
struct Ch0400RParameter;

/// `part2/ch04-01-r-revisited.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch04-01-r-revisited.md")]
struct Ch0401RRevisited;

/// `part2/ch04-02-providing.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch04-02-providing.md")]
struct Ch0402Providing;

/// `part2/ch04-03-widening-narrowing.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch04-03-widening-narrowing.md")]
struct Ch0403WideningNarrowing;

/// `part2/ch04-04-r-as-docs.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch04-04-r-as-docs.md")]
struct Ch0404RAsDocs;

/// `part2/ch05-01-positional-problem.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch05-01-positional-problem.md")]
struct Ch0501PositionalProblem;

/// `part2/ch05-02-tags.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch05-02-tags.md")]
struct Ch0502Tags;

/// `part2/ch05-03-context-hlists.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch05-03-context-hlists.md")]
struct Ch0503ContextHlists;

/// `part2/ch05-04-get-getmut.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch05-04-get-getmut.md")]
struct Ch0504GetGetmut;

/// `part2/ch06-01-what-is-layer.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch06-01-what-is-layer.md")]
struct Ch0601WhatIsLayer;

/// `part2/ch06-02-building-layers.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch06-02-building-layers.md")]
struct Ch0602BuildingLayers;

/// `part2/ch06-03-stacking.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch06-03-stacking.md")]
struct Ch0603Stacking;

/// `part2/ch06-04-layer-graphs.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch06-04-layer-graphs.md")]
struct Ch0604LayerGraphs;

/// `part2/ch07-01-service-traits.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-01-service-traits.md")]
struct Ch0701ServiceTraits;

/// `part2/ch07-02-service-env.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-02-service-env.md")]
struct Ch0702ServiceEnv;

/// `part2/ch07-03-providing-services.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-03-providing-services.md")]
struct Ch0703ProvidingServices;

/// `part2/ch07-04-complete-example.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-04-complete-example.md")]
struct Ch0704CompleteExample;

/// `part2/ch07-05-tokio-bridge.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-05-tokio-bridge.md")]
struct Ch0705TokioBridge;

/// `part2/ch07-06-platform-services.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-06-platform-services.md")]
struct Ch0706PlatformServices;

/// `part2/ch07-07-reqwest-http.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-07-reqwest-http.md")]
struct Ch0707ReqwestHttp;

/// `part2/ch07-08-axum-host.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-08-axum-host.md")]
struct Ch0708AxumHost;

/// `part2/ch07-09-tower-service.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-09-tower-service.md")]
struct Ch0709TowerService;

/// `part2/ch07-10-config.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-10-config.md")]
struct Ch0710Config;

/// `part2/ch07-11-logger.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-11-logger.md")]
struct Ch0711Logger;

/// `part2/ch07-12-durable-workflow.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-12-durable-workflow.md")]
struct Ch0712DurableWorkflow;

/// `part2/ch07-12-opentelemetry.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-12-opentelemetry.md")]
struct Ch0712Opentelemetry;

/// `part2/ch07-12-rpc-boundaries.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part2/ch07-12-rpc-boundaries.md")]
struct Ch0712RpcBoundaries;
