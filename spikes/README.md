# spikes/: 0.7 kernel design spikes and their shared harness

Temporary. This directory hosts the two kernel spikes that ruling R1 runs in parallel, plus the harness
both are measured with. `w2-spike-gate` reads the numbers into `docs/benchmarks/SPIKE-REPORT.md` for
the human design choice **H1**, and `k-rebuild` deletes `spikes/**` once the chosen kernel lands in
`id_effect::next`.

| Path | Owner | What |
|---|---|---|
| `Cargo.toml` | w2-spike-harness | standalone workspace: `corpus`, `kernel_a`, `kernel_b` |
| `corpus/` | w2-spike-harness | `handlers.md` (the 21 shapes), `README.md` (markup + counting + fairness rules), crate `spike_corpus` (shared services) |
| `scripts/count-annotations.sh` | w2-spike-harness | inference-annotation counts per corpus shape |
| `scripts/gen-caps.sh` | w2-spike-harness | generates the CAPS x HANDLERS compile-time stress crate |
| `scripts/check-time.sh` | w2-spike-harness | `cargo check` wall time, median of 3 |
| `scripts/templates/{plain,v06}.sh` | w2-spike-harness | gen-caps templates: no-kernel floor, and the 0.6 baseline |
| `kernel_a/` | k-spike-a | Design A, `.maestro/specs/0.7/kernel-design-a-hlist.md` |
| `kernel_b/` | k-spike-b | Design B, `.maestro/specs/0.7/kernel-design-b-traitenv.md` (a standalone crate, not `kernel_v2` in core) |

## Workspace

`spikes/` is its own cargo workspace, like `labs/`, and is **not** a member of the root workspace. The
root `cargo check --workspace`, coverage (`scripts/llvm-cov-nextest.sh`) and moon CI therefore never
build spike code. The spikes may depend on `crates/*` by path if they want to reuse kernel-neutral
modules (for example `id_effect`'s `Cause`/`Defect`), but neither is required. `Cargo.lock` and `target/`
stay local, as in `labs/`. Run everything from this directory:

```bash
cd spikes
devenv shell -- cargo check --workspace
devenv shell -- cargo nextest run -p kernel_a     # or kernel_b
```

## What each spike crate must provide

The paths are inside each spike's own contract (`spikes/kernel_{a,b}/**`).

1. **`tests/corpus.rs`**: every shape in [`corpus/handlers.md`](corpus/handlers.md), each in a
   `// corpus:begin <ID>` … `// corpus:end <ID>` region with a `#[test]` asserting the shape's behaviour,
   or declared `// corpus:unsupported <ID> <reason>`. The rules are in [`corpus/README.md`](corpus/README.md).
   `spike_corpus` is already a dev-dependency of both crates.
2. **`harness/gen-caps.sh`**: the gen-caps template for the design (interface below). `gen-caps.sh`
   resolves `--template a` / `--template b` to `spikes/kernel_{a,b}/harness/gen-caps.sh`.
3. **`tests/ui/*.rs` + `.stderr`**: trybuild cases. Per the release plan these are at least missing
   capability, duplicate/ambiguous and wrong provide, plus design-specific ones.
4. **`docs/benchmarks/spike-{a,b}.json`**: bench results in the `scripts/bench-export.sh` format (from
   `b0-bench-tooling`), so that `scripts/bench-compare.sh docs/benchmarks/v0.6.json
   docs/benchmarks/spike-{a,b}.json` applies the D9 gates.

The R1 shared ideas are mandatory in both spikes: an inline `Ready` variant, `Cause<E>` as the internal
error channel, a one-shot completion slot (no `watch`), and `effect_local!`/`LocalEffect`.

## The measurements `w2-spike-gate` collects

| Figure | Command (from the repo root) |
|---|---|
| annotation counts | `bash spikes/scripts/count-annotations.sh --json spikes/kernel_a/tests/corpus.rs` (same for b) |
| B's forward-list size | the `fwd` column/summary of the same run on `kernel_b` (W01 is the showcase-sized env) |
| `cargo check` at 16 caps x 100 handlers | `bash spikes/scripts/check-time.sh --gen a 16 100 --json` (same for b) |
| the 0.6 equivalent | `ID_EFFECT_PATH=$PWD/.tmp/bench-v0.6/crates/id_effect bash spikes/scripts/check-time.sh --gen v06 16 100 --json` |
| the no-kernel floor | `bash spikes/scripts/check-time.sh --gen plain 16 100 --json` |

Prefix each command with `devenv shell --`. Run all check-time figures in one session on the
reference machine. Design A's compile-time falsifier is A's median above 2x the `v06` median. Each
script documents itself (`--help`) and has a `--self-test`; `gen-caps.sh` also has `--dry-run`.

## gen-caps templates

`gen-caps.sh CAPS HANDLERS --template T` generates the same handler *plan* for every template. Handler
`i` needs 1 + i % 5 distinct capabilities, in a scattered, unsorted order. Every handler with
`i % 4 == 3` also calls handler `i - 2`, so its `R` is the union of both sets (a subset call:
narrowing in A, bound entailment in B). For 16x100 that gives 25 composite handlers and 1 to 8 caps per
handler. Only the *spelling* comes from the template.

A template is a bash file that `gen-caps.sh` sources. It must define these six functions, each writing
Rust (or TOML) to stdout:

| Function | Writes |
|---|---|
| `tpl_dependencies` | lines for the generated crate's `[dependencies]`, e.g. `kernel_a = { path = "${SPIKES_DIR}/kernel_a" }` |
| `tpl_prelude` | `use` items and shared items at the top of `src/lib.rs` |
| `tpl_cap NAME INDEX` | the capability type `NAME`, whose value is `INDEX` (u64), e.g. `pub struct Cap07(pub u64);`, plus any impls the design needs |
| `tpl_env CAP...` | whatever the design needs for the full app environment over all caps (a type alias, a `#[derive(Env)]` struct, a layer) |
| `tpl_handler NAME CALLEE CAP...` | `pub fn NAME…`: an effect whose `R` is exactly `CAP...` (in that order), that reads every cap and returns their sum. If `CALLEE` is not `-`, it also runs `CALLEE` (whose caps are a subset of `CAP...`) and adds its result |
| `tpl_edge HANDLER...` | `pub fn edge() -> u64`: builds the full env (all caps in `CAP_NAMES`, `CapNN` = `NN`), runs every handler against it, and returns the sum (it owns its runtime, so no `async`) |

Available to templates: `CAP_NAMES` (bash array), `SPIKES_DIR`, `REPO_ROOT`, `ID_EFFECT_PATH`,
`GEN_CAPS`, `GEN_HANDLERS`, `GEN_TEMPLATE`. Write handlers the way a user of the design would: generic
`R: Has<..>` bounds for B (B1 style), `caps!(..)` for A. The generated crate gets a `#[cfg(test)]`
asserting `edge()`, so prove a template with:

```bash
devenv shell -- bash spikes/scripts/gen-caps.sh --self-test --template a     # 4x10, runs cargo test
```

`scripts/templates/plain.sh` and `scripts/templates/v06.sh` are worked examples.
