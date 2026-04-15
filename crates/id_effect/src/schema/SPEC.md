# Stratum 13 — `schema/` submodule specification (@TESTING.md)

This document specifies the **Schema**, **Data**, and related **structural validation** subsystem implemented under [`crates/id_effect/src/schema/`](./). It is a **normative design and delivery plan** for reaching **feature parity** with **Effect.ts `@effect/schema`** (as exposed from the **`effect`** npm package’s `Schema` module), and for **optional extensions** idiomatic to Rust.

**Authoritative testing contract:** All behavior described here is **not delivered** until it is covered by tests that obey the repository-root **[`TESTING.md`](../../../../TESTING.md)** (relative to this file: four levels up to the monorepo root). That document governs **TDD/BDD naming**, **module test trees**, **`rstest`**, **branch and mutation coverage**, **async tests**, and **integration** policy. This spec explicitly references **`@TESTING.md`** in every section heading as a reminder that **no merge is complete without `TESTING.md` compliance.**

**Related specifications:** The parent crate overview is [`../../SPEC.md`](../../SPEC.md). Foreign-language correspondence (Haskell, F#, Effect.ts) is summarized both there and below where it affects Schema.

---

## 1. Goals and non-goals (@TESTING.md)

### 1.1 Primary goals (@TESTING.md)

- **Parity:** Provide a **primary**, **idiomatic Rust API** for defining **schemas**—bidirectional **decode** / **encode** between a **semantic type** `A` and a **wire / encoded type** `I`, plus **decoding from dynamically shaped trees** (`Unknown` today)—such that typical **Effect.ts `Schema` examples** (structs, unions, transforms, recursion, refinements) have a **direct, documented Rust equivalent**.
- **Data:** Keep **`EffectData`** (derive via `id_effect_proc_macro::EffectData`) as the **standard “dataclass” story** (structural `PartialEq` / `Eq` / `Hash`); **integrate** schemas so users do not maintain two unrelated models.
- **Single source of truth:** Schemas should be the **default** way to declare validated structured data at boundaries (config, IPC, JSON), superseding ad-hoc parsing where feasible.

**Delivery (`@TESTING.md`):** For each goal, maintain **acceptance tests** named under **BDD conventions** in [`TESTING.md`](../../../../TESTING.md) (§BDD-Style Test Naming): e.g. `decode_when_input_matches_schema_returns_ok`, `round_trip_when_encode_then_decode_preserves_value`. Track parity scenarios in a **dedicated test module tree** (§Module Test Trees).

### 1.2 Non-goals (initially) (@TESTING.md)

- **Full JSON Schema 2020-12** as a **guaranteed** output (Effect.ts also stops at transforms for JSON Schema generation); any export is **best-effort** and **explicitly documented** for gaps.
- **Automatic OpenAPI** generation (may build on JSON Schema export later).
- **Proof** of categorical laws in a proof assistant—laws are **test contracts** per [`TESTING.md`](../../../../TESTING.md) (§Laws-as-tests / algebra patterns in parent SPEC).

**Delivery (`@TESTING.md`):** Non-goals must be **asserted in tests** that **do not** claim unsupported behavior—e.g. tests for `json_schema_export` skip or expect `UnsupportedTransform` variants rather than weakening coverage gates.

---

## 2. Normative references and terminology (@TESTING.md)

### 2.1 External references (@TESTING.md)

- **Effect.ts `Schema`:** The **`effect`** package’s `Schema` API (e.g. `Schema.Struct`, `Schema.Union`, `Schema.transform`, `Schema.suspend`, `Schema.Class`, `decodeUnknown`, `encode`, JSON Schema helpers). Parity is **behavioral** and **documented**, not a line-by-line port.
- **Repository testing:** **[`TESTING.md`](../../../../TESTING.md)** at the monorepo root—**authoritative** for how tests are written, organized, and counted toward coverage.

**Delivery (`@TESTING.md`):** Parity examples ported from Effect.ts **must** live under `#[cfg(test)]` modules with **rstest** tables where the same assertion applies to many inputs (§rstest).

### 2.2 Terminology (@TESTING.md)

| Term | Meaning |
|------|---------|
| **Semantic type `A`** | The type used in application logic after a successful decode (e.g. `User`, `NonZeroU32`). |
| **Wire / encoded type `I`** | The type before transformation or as stored on the wire (e.g. `String` for ISO dates, `i64` for JSON integers). |
| **Tag `E` (`EffectData`)** | Phantom or marker used with `Schema<A, I, E>` today—may evolve; must remain **`EffectData`**-compatible where required by constructors. |
| **`Unknown`** | Dynamically shaped tree (JSON-like) for `decode_unknown` without pulling in `serde` in the core. |
| **Refinement** | Predicate or validation step after decode (Effect.ts `Schema.filter`, `pipe`, branded refinements). |

**Delivery (`@TESTING.md`):** Terminology regressions (wrong `I` for a primitive) are caught by **round-trip** and **decode_unknown** tests per field (§Mutation Coverage Checklist).

---

## 3. Current implementation snapshot (@TESTING.md)

### 3.1 Modules and responsibilities (@TESTING.md)

| Module / file | Role |
|---------------|------|
| [`mod.rs`](./mod.rs) | Public exports and stratum index. |
| [`parse.rs`](./parse.rs) | `Schema<A, I, E>`, `Unknown`, `ParseError`, combinators (`i64`, `string`, `bool_`, `f64`, `transform`, `filter`/`refine`, `optional`, `array`, `tuple`, `tuple3`, `struct_`, `struct3`, `union_`). |
| [`data.rs`](./data.rs) | `EffectData`, `DataStruct`, `DataTuple`, `DataError`. |
| [`brand.rs`](./brand.rs) | `Brand`, `RefinedBrand`—nominal / refined newtypes. |
| [`equal.rs`](./equal.rs) | `Equal`, `EffectHash`—not schema-derived equivalence. |
| [`order.rs`](./order.rs) | Ordering helpers. |

**Delivery (`@TESTING.md`):** Each public symbol in these modules requires **branch coverage** on success and failure paths (§Branch Coverage, §Mutation Coverage). New exports extend the same obligation.

### 3.2 Known limitations (gap list driving this spec) (@TESTING.md)

- **Arity:** `struct_` / `tuple` cover **two** components; `struct3` / `tuple3` cover **three**. Effect.ts allows **arbitrary** arity—**derive** / **macros** / **HList** still needed for general structs and longer tuples without combinator explosion.
- **Primitives:** Core exposes **`i64`**, **`String`**, **`bool_`**, **`f64`** (see tests in `parse.rs` per **`TESTING.md`**). **Integer width** policy for JSON (`I64` only in `Unknown`) is unchanged; very large integers may still stringify at the JSON bridge.
- **`Unknown`:** Has **`Null`**, **`Bool`**, **`I64`**, **`F64`**, **`String`**, **`Array`**, **`Object`**. JSON bridges (`id_effect_axum`, `id_effect_reqwest`) map non-integer **`serde_json::Number`** values to **`F64`**.
- **Unions:** `union_` is **binary try/fallback** with the **same semantic type** and **`Unknown` wire**—not a general **sum type** or **discriminated union**.
- **Recursion:** No **`suspend`** / lazy schema for recursive ADTs.
- **Async / `Effect`:** Decoders are **synchronous** `Result`; Effect.ts supports **async** / **`Effect`-returning** `transformOrFail`.
- **Equivalence:** No **`Schema.equivalence`** derived from schema AST; `equal.rs` is **std-based**, not schema-driven.
- **Annotations / metadata:** No `title` / `description` / `examples` / custom equivalence hooks on schemas.
- **JSON Schema export:** Not implemented.

**Delivery (`@TESTING.md`):** Each limitation closed by an implementation **must** include **regression tests** that would have failed on the old API (§Output Mutations)—e.g. a **third field** in a struct schema test once n-ary structs exist.

---

## 4. Design principles (@TESTING.md)

### 4.1 Preserve `Schema<A, I, E>` as the semantic core (@TESTING.md)

- **Bidirectional codecs** stay first-class: **decode**: `I → Result<A, ParseError>`, **encode**: `A → I`, **decode_unknown**: `&Unknown → Result<A, ParseError>`.
- **Separate `A` and `I`** to model Effect.ts’s **Type vs Encoded** split (e.g. `Date` vs ISO string).

**Delivery (`@TESTING.md`):** **Round-trip laws** (`encode` ∘ `decode` ≈ `id` on valid inputs; `decode` ∘ `encode` ≈ `id`) for **pure** schemas belong in **`rstest`**-driven tables (§rstest, §Boundary Testing).

### 4.2 Unknown interchange vs serde boundary (@TESTING.md)

- Keep **`Unknown`** (or an evolved tree) as the **serde-optional** interchange for dynamic data.
- Provide an explicit **bridge** to `serde_json::Value` in a **non-core** module or feature when needed.

**Delivery (`@TESTING.md`):** Bridge functions need **both** unit tests and, if they touch IO, **integration** tests marked per **§Integration Tests** (optional `#[ignore]` with documented rationale).

### 4.3 Macros and derives as the primary UX (@TESTING.md)

- **Declarative** APIs for small arity + **`#[derive(Schema)]`** (or a dedicated `effect-schema-derive` crate) for structs and enums—Rust’s answer to `Schema.Struct` and `Schema.Class`.

**Delivery (`@TESTING.md`):** Proc-macro tests follow **§Helper Functions & Fixtures**; use **UI tests** (`trybuild`) if the crate splits, per workspace patterns and [`TESTING.md`](../../../../TESTING.md).

### 4.4 Testing is the definition of done (@TESTING.md)

- No feature in this spec is **complete** without tests aligned to **[`TESTING.md`](../../../../TESTING.md)**—including **coverage** expectations (§Coverage Goals) and **no relaxation** of gates to pass broken behavior.

**Delivery (`@TESTING.md`):** Schema changes run under **`moon run :coverage`** or equivalent CI; failing gates **fix code/tests**, not thresholds (see parent SPEC and [`TESTING.md`](../../../../TESTING.md)).

---

## 5. Parity matrix (Effect.ts vs Rust) (@TESTING.md)

### 5.1 Structural combinators (@TESTING.md)

| Feature | Effect.ts | Rust (planned / current) |
|---------|-----------|---------------------------|
| Struct / record | `Schema.Struct({ … })` | **Planned:** n-ary struct / derive |
| Tuple | `Schema.Tuple(…)` | **Current:** 2-ary; **planned:** n-ary |
| Arrays | `Schema.Array(S)` | **Current:** `array` |
| Records / maps | `Schema.Record` / keyed | **Planned:** string-keyed map schema |
| Optional / null | `Schema.optional`, `NullOr`, etc. | **Current:** `optional`; **planned:** null literal, policies |
| Union | `Schema.Union(…)`, discriminated | **Current:** binary `union_`; **planned:** n-ary + discriminant |

**Delivery (`@TESTING.md`):** One **parity test file** (or module tree) per row family—**nested modules** per combinator (§Module Test Trees Pattern 1).

### 5.2 Primitives and refinements (@TESTING.md)

| Feature | Effect.ts | Rust (planned / current) |
|---------|-----------|---------------------------|
| Strings, numbers, bool | `Schema.String`, `Number`, `Boolean` | **Extend** primitives + `Unknown` variants |
| Coercions | `NumberFromString`, `DateFromString`, … | **Planned:** transform schemas + prebuilt helpers |
| Refinements | `pipe`, `Schema.int`, `clamp`, brands | **Current:** `filter`/`refine`; **planned:** branded pipeline |

**Delivery (`@TESTING.md`):** **Boundary tests** for numeric ranges and invalid strings (§Boundary Testing); **rstest** for many string/number pairs.

### 5.3 Transformations and effects (@TESTING.md)

| Feature | Effect.ts | Rust (planned / current) |
|---------|-----------|---------------------------|
| `transform` | sync | **Current:** `transform` |
| `transformOrFail` | `Effect` / async | **Planned:** `transform_effect` / `Effect` integration at boundary |
| Recursion | `Schema.suspend` | **Planned:** lazy / boxed recursive schemas |

**Delivery (`@TESTING.md`):** **Async** tests use **`#[tokio::test]`** per §Async Tests in [`TESTING.md`](../../../../TESTING.md); sync path remains covered separately.

### 5.4 Data, classes, and nominal types (@TESTING.md)

| Feature | Effect.ts | Rust (planned / current) |
|---------|-----------|---------------------------|
| Data / equality | `Data` module | **`EffectData`** + derive |
| Classes | `Schema.Class` | **Planned:** derive + associated schema / `HasSchema` |
| Brands | `Brand` + schema | **Current:** `Brand` / `RefinedBrand`; **planned:** unified with `Schema` |

**Delivery (`@TESTING.md`):** **Equality/hash** tests for derived `EffectData` already exist—extend with **schema round-trips** on the same types.

### 5.5 Metadata and interop (@TESTING.md)

| Feature | Effect.ts | Rust (planned / current) |
|---------|-----------|---------------------------|
| Annotations | `annotations({ … })` | **Planned:** `Annotated<S, Meta>` or schema fields |
| Equivalence from schema | `Schema.equivalence` | **Planned:** derive or generate from AST |
| JSON Schema | `JSONSchema.make` | **Planned:** optional export module |

**Delivery (`@TESTING.md`):** Snapshot-style tests (if used) follow **§Snapshot Testing** in [`TESTING.md`](../../../../TESTING.md); JSON output tests must be **stable** and **deterministic**.

---

## 6. Proposed type and error model (@TESTING.md)

### 6.1 `Schema<A, I, E>` (@TESTING.md)

- **Invariant:** For **total** codecs on their domain, **decode** after **encode** is identity; **encode** after **decode** is identity on the **valid** subset of `I`.
- **Extension:** Optional **AST** or **vtable** for introspection (for equivalence / JSON Schema) behind an **opt-in** trait to avoid bloating every closure-based schema.

**Delivery (`@TESTING.md`):** Law tests colocated in `#[cfg(test)]` near `Schema` combinators; use **nested `mod`** per combinator (§Module Test Trees).

### 6.2 `Unknown` evolution (@TESTING.md)

- **Minimum:** Keep **`Bool`** as the dynamic carrier for booleans; add a **floating-point** variant or documented **lossy mapping** from JSON floats (e.g. **`F64(f64)`** vs rejecting non-integers at **`I64`**).
- **Primitives alignment:** Expose **`bool()`** / **`f64()`** (or **`number`**) schemas consistent with **`Unknown`** decoding rules.
- **Optional:** **Byte array** / **BigInt** for non-JSON protocols—feature-gated if needed.

**Delivery (`@TESTING.md`):** **Exhaustive `match`** tests when extending enums—**mutation coverage** on every new variant (§Mutation Coverage).

### 6.3 Parse errors (@TESTING.md)

- **Current:** `ParseError { path, message }`.
- **Planned:** **Issue list** or **tree** (multiple errors), optional **error codes**, **expected vs actual** hints for tooling—while keeping **string path** compatible for existing callers.

**Delivery (`@TESTING.md`):** **Assertion tests** on **path prefixing** (`prefix`, `prefix_index`) with **table-driven** cases; when multiple issues exist, test **ordering** and **stable** reporting.

---

## 7. Phased roadmap (@TESTING.md)

| Phase | Status | Notes |
|-------|--------|--------|
| 7.1 | **Mostly done** | Products through **arity 4** (`tuple4`, `struct4`); [`record`](./extra.rs); [`ParseErrors`](./parse_errors.rs); **no** `#[derive(Schema)]` yet. |
| 7.2 | **Partial** | [`suspend`](./extra.rs) implemented; **no** schema step that runs [`Effect`](../../kernel/effect.rs) internally. |
| 7.3 | **Partial** | [`HasSchema`](./has_schema.rs) implemented; **derive** deferred to `effect-proc-macro`. |
| 7.4 | **Open** | [`filter`](./parse.rs) / refinements; **RefinedBrand** + schema not wired. |
| 7.5 | **Partial** | Crate feature **`schema-serde`**: [`serde_bridge`](./serde_bridge.rs), [`json_schema_export`](./json_schema_export.rs) primitives only. |

### 7.1 Phase 1 — Kernel completeness (@TESTING.md)

- **Generalize products:** **Done** through **arity 4**; **derive** for arbitrary structs **not done**.
- **Extend `Unknown` and primitives:** **Done** (`bool_`, `f64`, `F64`, [`record`](./extra.rs)).
- **Errors:** [`ParseErrors`](./parse_errors.rs) for lists of [`ParseError`](./parse.rs); optional per-field **codes** still future work.

**Delivery (`@TESTING.md`):** Phase 1 exit = **all new combinators** covered per §Module Test Trees; **coverage** not regressed vs baseline (§Coverage Goals).

### 7.2 Phase 2 — Transformations and recursion (@TESTING.md)

- **`transform_or_fail` → `Effect`:** **Not done** (sync [`transform`](./parse.rs) only).
- **`suspend` / recursion:** [`suspend`](./extra.rs) **done** (`std::sync::OnceLock` cache).

**Delivery (`@TESTING.md`):** **Integration-style** tests for async transforms (`#[tokio::test]`); recursion tests with **depth limits** to avoid stack overflow in test (§Async Tests, §Branch Coverage).

### 7.3 Phase 3 — Data + Schema integration (@TESTING.md)

- **`#[derive(Schema)]`:** **Not done**.
- **`HasSchema` trait:** **Done** ([`has_schema.rs`](./has_schema.rs)).

**Delivery (`@TESTING.md`):** **UI / compile tests** if derives are added; **fixtures** for sample structs (§Helper Functions & Fixtures).

### 7.4 Phase 4 — Refinements and brands (@TESTING.md)

- **Composable refinements** with stable **error messages** and optional **JSON Schema** mapping.
- **`RefinedBrand` + `Schema`:** decode validates → construct brand.

**Delivery (`@TESTING.md`):** **Negative tests** for every rejection path (§Mutation Coverage Checklist).

### 7.5 Phase 5 — Interop and Rust-only extensions (@TESTING.md)

- **`serde_json::Value` ↔ `Unknown`:** **Done** with feature **`schema-serde`** ([`serde_bridge`](./serde_bridge.rs)); `effect-axum` / `effect-reqwest` delegate to it.
- **JSON Schema export:** **Primitive fragments** ([`json_schema_export`](./json_schema_export.rs)) only.
- **Extensions:** **`no_std`** subset discussion, **const** metadata where feasible.

**Delivery (`@TESTING.md`):** **Feature-gated** tests (`#[cfg(feature = "…")]`) with **documented** CI matrix; **§Integration Tests** for large JSON files if any.

---

## 8. `EffectData` and the “dataclass” story (@TESTING.md)

### 8.1 Role of `EffectData` (@TESTING.md)

- **`EffectData`** remains the **canonical** structural equality/hash marker for **domain values** (maps, sets, services per crate patterns).
- **Schemas** must **not** replace `EffectData`; they **validate at boundaries** and **produce** `EffectData` values.

**Delivery (`@TESTING.md`):** Tests that **insert decoded values into `HashMap`** / **`HashSet`** keys (see existing `data.rs` tests) remain **green** after integration.

### 8.2 `DataStruct`, `DataTuple`, `DataError` (@TESTING.md)

- Keep **newtype wrappers** for opaque “data” values where nominal distinction matters.
- **`DataError`:** errors that are also `EffectData`—unchanged contract.

**Delivery (`@TESTING.md`):** **Behavior-driven** tests for wrappers (equality/hash) per [`TESTING.md`](../../../../TESTING.md) §BDD-Style Test Naming.

---

## 9. Risks and mitigations (@TESTING.md)

### 9.1 API surface explosion (@TESTING.md)

- **Risk:** Too many tuple overloads.
- **Mitigation:** Prefer **derive** + **single** generic builder over arity overloads.

**Delivery (`@TESTING.md`):** **Public API** tests or **doc tests** (if used) must compile—see §Doc Tests in [`TESTING.md`](../../../../TESTING.md) if enabled.

### 9.2 JSON Schema mismatch (@TESTING.md)

- **Risk:** Users expect **full** JSON Schema semantics.
- **Mitigation:** Document **subset** and **transform stops** export (mirror Effect.ts behavior).

**Delivery (`@TESTING.md`):** **Explicit** tests for **unsupported** export cases—**must not** silently produce wrong schemas.

### 9.3 Performance (@TESTING.md)

- **Risk:** Layered boxed closures and error allocation.
- **Mitigation:** **Benchmarks** optional; **fast paths** for known primitives.

**Delivery (`@TESTING.md`):** If benchmarks added, they live under **bench** with **non-flaky** thresholds; unit tests remain **fast** (§Performance / CI norms in parent docs).

---

## 10. Success criteria (@TESTING.md)

### 10.1 Parity acceptance (@TESTING.md)

- A **catalog** of Effect.ts Schema examples (structs, unions, transforms, suspend) each has a **Rust counterpart** in **tests** or **examples** with **passing** decode/encode/`decode_unknown` round-trips.
- **`TESTING.md`** conventions satisfied: **BDD names**, **module trees**, **`rstest`** where appropriate, **coverage** goals met.

### 10.2 Documentation (@TESTING.md)

- This **`SPEC.md`** and **`../../SPEC.md`** cross-link; **module-level `//!` docs** in `schema/*.rs` summarize public API.
- **Examples** in `crates/id_effect/examples/` reference new combinators with **runnable** code.

**Delivery (`@TESTING.md`):** **Doc examples** are **tested** if the workspace uses **doctest**—otherwise **example binaries** serve as the contract (verify with `cargo test -p id_effect` / CI).

---

## 11. File and module layout (target) (@TESTING.md)

### 11.1 Suggested future layout (@TESTING.md)

| Path | Purpose |
|------|---------|
| `parse.rs` / `schema_core.rs` | Core `Schema`, combinators |
| `unknown.rs` | `Unknown` + conversions |
| `error.rs` | `ParseError` / `ParseIssue` |
| `derive` (proc-macro crate) | `derive(Schema)` |
| `json_schema.rs` (feature) | JSON Schema export |
| `serde_bridge.rs` (feature) | `serde_json` interop |

**Delivery (`@TESTING.md`):** **Each new file** gets its own **`#[cfg(test)] mod tests`** tree (§Test File Structure).

---

## 12. Traceability to [`TESTING.md`](../../../../TESTING.md) (@TESTING.md)

### 12.1 Section mapping (@TESTING.md)

| This spec concern | `TESTING.md` section to apply |
|-------------------|------------------------------|
| Naming tests | §BDD-Style Test Naming |
| Organizing tests | §Module Test Trees, §Test File Structure |
| Tables / duplicates | §rstest |
| Branches / mutations | §Branch Coverage, §Mutation Coverage Checklist |
| Async decoders | §Async Tests |
| External systems | §Integration Tests |
| Coverage gates | §Coverage Goals |

**Delivery (`@TESTING.md`):** Reviewers use this table as a **checklist** for every PR touching `crates/id_effect/src/schema/`.

---

### Document control (@TESTING.md)

- **Version:** 1.0 (planning specification; implementation may lag sections until phases complete).
- **Owners:** `effect` crate maintainers; **tests** are the **source of verification** per [`TESTING.md`](../../../../TESTING.md).

**Delivery (`@TESTING.md`):** When implementation lands, **update this SPEC** to mark sections **implemented** and point to **exact test modules** that enforce them—still following **`@TESTING.md`** in headings for consistency.
