# Code analytics

Checks for where the code is getting overloaded or tangled: size, complexity,
cycles between modules, fan-in/fan-out, types that hold or do too much, hotspots and
change coupling. They run from the command line and write plain text; nothing here
runs in CI yet.

```sh
cargo install --locked tokei rust-code-analysis-cli cargo-modules
scripts/analytics/run.sh                 # whole history
scripts/analytics/run.sh --since 2026-06-01
```

Results go to `target/analytics/` (one `.txt` per analysis) and to the terminal. It
takes about five minutes; `rust-code-analysis` and `cargo-modules` do most of the
work. code-maat (Java) is downloaded to `target/analytics/` on first use.

## The analyses

| Analysis | Tool | Script | Question it answers |
|---|---|---|---|
| Size | [tokei](https://github.com/XAMPPRocky/tokei) | `run.sh` | How big is each crate? |
| Complexity | [rust-code-analysis](https://github.com/mozilla/rust-code-analysis) | `complexity.py` | Which functions are hard to follow, which files have the most to follow? |
| Module cycles | [cargo-modules](https://github.com/regexident/cargo-modules) | `cycles.py` | Do modules depend on each other in a circle? |
| Fan-in / fan-out | — | `fan.py` | Which modules are foundations, which are hubs? |
| Type shapes | — | `shapes.py` | Which types hold (fields) or do (methods) too much? |
| Hotspots | [code-maat](https://github.com/adamtornhill/code-maat) | `hotspots.py` | Which big files change most often? |
| Change coupling | code-maat | `run.sh` | Which files always change together? |

### Complexity

- **Cognitive complexity** per function: how hard it is to follow; nesting counts
  extra. Over ~15 is worth a look, over ~25 is hard to review.
- **Cyclomatic complexity**: paths through a function, i.e. how many tests it takes to
  cover.
- **Per file**: the sum of its functions' cognitive complexity (how much there is to
  hold in your head) and the worst one. Closures count as functions.
- rust-code-analysis's maintainability index isn't used: for files it bottoms out at
  0 from ~400 lines on, so it tells nothing.

### Module cycles

`cargo modules dependencies`, filtered to modules and their `use` edges, then
`cycles.py` finds the strongly connected components.

- `cargo modules --acyclic` isn't used directly: it also counts a type and its own
  methods as a cycle.
- A module's edges to its own parent and children are left out: one type split over
  files (`use super::…` in the child, `pub use child::…` in the parent) is not a cycle.
- `cargo modules` only writes its graph when its output is piped.

A cycle means the modules can't be understood, tested or moved one at a time.

### Fan-in / fan-out

Per module (a crate's top-level module; files in a folder belong to it, re-exported
items count for the crate root):

- **fan-in**: how many modules use it; **fan-out**: how many it uses.
- **instability** = out / (in + out): near 0 for foundations, near 1 for the edges
  that assemble everything.
- **hub** (flagged): fan-in and fan-out both ≥ 5. Changes ripple both ways.

Many users alone is fine for a small, stable module (`lexer`: fan-in 9, fan-out 0).
Being used a lot is a problem when the module also uses a lot, is big, or changes
often.

Limit: items re-exported at a crate's root count for the root (`tangul_engine::Approval`
comes from `engine::gate`, but counts for `engine::(root)`).

### Type shapes

Named fields per struct, and `fn`s over all `impl` blocks of a type (across files, so
a type split over several files counts as one). A type with dozens of fields or
methods is several things in one; splitting its files doesn't split it.

### Hotspots and change coupling

From `git log` through code-maat:

- **Hotspots**: changes × current lines, relative to the top file. Big files that
  change constantly are where size costs time on every change: the first place to
  split.
- **Change coupling**: pairs of files changed in the same commits; degree = the share
  of their commits they share (≥ 50 %, both files changed ≥ 4 times). Expected
  between a trait and its implementations; a surprise between unrelated files means
  a boundary is in the wrong place.

History signals get better with more commits; with a short history, read them as hints.

## Reading the results together

- A file that is a **hotspot**, **complex** and whose type has many **fields and
  methods** is overloaded: split it by responsibility, not by line count.
- A **cycle** between modules, or **coupling** between files in different modules,
  says where a boundary is drawn wrong.
- High **fan-in** with low **fan-out** is a healthy foundation; keep it small and
  stable.

## Also useful

- **Clippy limits** (`clippy.toml`): `too_many_lines`, `cognitive_complexity`,
  `too_many_arguments`, `type_complexity`, `struct_excessive_bools`. With the existing
  `-D warnings` they would stop new overload at lint time.
- **cargo-machete** and **cargo-deny** (in `make lint`): unused dependencies,
  duplicate versions, banned crates.
- **cargo-udeps**: finds unused dependencies more precisely than cargo-machete, but
  needs nightly Rust (the compiler's unstable daily build).
