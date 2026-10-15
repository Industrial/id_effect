# Introduction

Welcome to *Typed Effects in Rust*.

If you already use async Rust, you know the model: `Future`s are polled by an executor; work runs when those futures are driven (for example with `.await`). That foundation is sound, and this book does not ask you to unlearn it.

What teams often hit next is **organization at scale**: error types that grow without structure, dependencies threaded through long call chains, and background work whose lifetime is hard to reason about. Those problems are not unique to Rust, but they show up in every non-trivial async codebase.

**id_effect** is a library for writing async programs where the **shape** of the work—success type, error type, and required environment—is carried in one place, and where much of the program is built as **composable descriptions** (`Effect<A, E, R>`) that you run only when you choose how and with which dependencies.

You still run on ordinary async runtimes. You still use `.await` inside bridges to third-party code. What changes is how you **structure** domain logic, tests, and dependency boundaries.

## Who This Book Is For

You should know Rust basics: ownership, borrowing, traits, and how `async`/`await` and `Future` fit together. You do not need prior experience with category theory or functional programming jargon—we introduce terms only when they help.

If you want a **typed, compositional** style for async Rust—with explicit requirements in the type system and a clear split between “what to run” and “how to run it”—this book is for you.

## How to Read This Book

**Part I: Foundations** explains why effects are useful and teaches the core types. Start here.

**Part II: Environment & Dependencies** covers the `R` parameter and compile-time dependency injection patterns, then walks the **workspace integration crates** (Tokio bridge, platform I/O, reqwest, Axum, Tower, config, logging) so you can wire a real binary without leaving the book.

**Part III: Real Programs** covers error handling, concurrency, resources, and scheduling for production code.

**Part IV: Advanced** covers STM, streams, schemas, and testing—read when you need those topics.

This edition describes **id_effect 0.7**: `Effect<A, E, R>` is `Send`, `R` is a typed list of capabilities (a missing one is a compile error), dependencies are built with `Layer`s, and programs run at the edge with `Effect::block_on` (or `Runtime::run(..).await` inside async code). If you are upgrading from 0.6, see [the 0.7 migration guide](https://github.com/Industrial/id_effect/blob/main/docs/MIGRATION-0.7.md).

Code examples show the shape of real code; small helpers and domain types they mention are left out. The runnable versions live in the crate's `examples/` and `tests/`.

Let's begin.
