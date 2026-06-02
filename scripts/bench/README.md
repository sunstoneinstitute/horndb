# HornDB vs RDFox — internal benchmark harness

`compare-rdfox.sh` times HornDB and RDFox on **identical input files** for the
three operations HornDB has published performance goals against RDFox for, and
prints a side-by-side table with the goal verdict.

> **Licence note.** RDFox commercial/evaluation licences forbid *publishing*
> comparative benchmark numbers (the "DeWitt clause"). This harness is for
> **internal** use only — see the baseline note in [`../../BENCHMARKS.md`](../../BENCHMARKS.md).
> Keep the numbers it prints out of public docs, commit messages, and issues.

## What it measures

| Comparison    | HornDB path                                          | Goal (BENCHMARKS.md)            | Apples-to-apples? |
|---------------|------------------------------------------------------|---------------------------------|-------------------|
| `import`      | `horndb_storage` N-Triples bulk loader               | SPEC-02 F8 — ≥ 1 M triples/sec  | exact (same file) |
| `transitive`  | `horndb_closure` GraphBLAS transitive closure        | SPEC-05 acc#1 — ≥ 10× RDFox     | exact closure (count cross-checked) |
| `materialize` | `horndb_owlrl` OWL 2 RL forward materialization       | Stage-1 gate — within 3× RDFox  | indicative (see caveat) |

**Reasoning is isolated from parsing.** For `transitive` and `materialize` the
data file is imported into RDFox first with no rules present, *then* the rule
file is imported — so the rule-import time RDFox reports is materialization
only. HornDB reports its reasoning phase separately from parsing the same way.

## Prerequisites

- The RDFox distribution zip and an evaluation/benchmarking licence file.
  Defaults assume `~/Downloads/RDFox-macOS-arm64-7.5b.zip` and
  `~/Downloads/RDFox.lic`; override with `RDFOX_ZIP` / `RDFOX_HOME` and
  `RDFOX_LIC`.
- A working HornDB build toolchain (the script builds `horndb-bench`, which
  pulls the vendored GraphBLAS — already cached after a normal workspace build).
- `python3` (stdlib only) for workload generation.

## Usage

```bash
# defaults: chain=2500, taxonomy depth=30 / instances=20000
scripts/bench/compare-rdfox.sh

# bigger workloads; keep generated files for inspection
scripts/bench/compare-rdfox.sh --chain 10000 --instances 100000 --keep

# point at an already-unpacked RDFox to skip the unzip
RDFOX_HOME=/path/to/RDFox-macOS-arm64-7.5b scripts/bench/compare-rdfox.sh
```

Flags: `--chain N`, `--depth D`, `--instances I`, `--keep`.

Output: a summary table on stdout, **and** a persisted copy of each run under
`scripts/bench/results/run-<timestamp>/` (summary + raw JSON + RDFox logs).

> **`scripts/bench/results/` is gitignored on purpose.** RDFox numbers must not
> be committed to a public repo — that counts as publishing. The directory ships
> with a `.gitignore` that excludes all its contents, and the script refuses
> quietly only after warning if you redirect `RESULTS_DIR` somewhere git would
> track. Scratch files (the RDFox unpack, generated workloads) stay under
> `target/bench-rdfox/`, which is already ignored.

## How the pieces fit

```
gen_workload.py ──┬─► chain.nt ──────────► horndb-bench transitive ─┐
                  │              └────────► RDFox import + trans rule┤
                  ├─► tax.nt   ──┬───────► horndb-bench materialize ─┤─► compare-rdfox.sh
                  │              └───────► RDFox import + owl2rl rule┤      (table)
                  └─► (lubm1/chain) ──────► horndb-bench import ──────┘
                                  └────────► RDFox import
```

- `gen_workload.py` — emits canonical N-Triples so the exact same bytes feed
  both engines. `chain N` is a single-predicate path (closure = N·(N−1)/2);
  `taxonomy D I` is an `rdfs:subClassOf` chain of depth `D` with `I` instances
  typed at the bottom class (drives `cax-sco` + `scm-sco`).
- `rules/owl2rl-core.dlog` — the two OWL 2 RL rules (`cax-sco`, `scm-sco`) the
  taxonomy workload exercises, in RDFox Datalog, so RDFox runs the same closure.
- `horndb-bench` (`crates/bench-rdfox`) — the HornDB-side micro-runner. One
  subcommand per comparison; each prints a single line of JSON.

## Caveats (read before trusting a number)

- **`materialize` is indicative, not a closure-equality test.** Each engine runs
  its own OWL 2 RL. HornDB additionally injects a fixed XSD datatype-lattice
  base (~60 triples) on every load, so totals differ by that constant — the
  cross-check line prints the delta. At the default sizes it is < 0.05% of the
  closure. RDFox here runs only the two rules in `owl2rl-core.dlog`; HornDB runs
  its whole Stage-1 rule set, but on this vocabulary only those two fire.
- **`transitive` measures different end states.** HornDB computes the closure as
  a GraphBLAS matrix and reports `nvals`; RDFox materializes the closure triples
  into its store. The reported HornDB time is matrix-build + closure-compute.
  The closure *sizes* are cross-checked for equality; the *work* RDFox does is
  broader, so the ratio flatters HornDB — it matches the SPEC-05 acc#1 framing
  ("the closure is faster than RDFox"), not a like-for-like store materialization.
- **Wall-clock, single run.** No warmup, no repeats, no criterion intervals.
  For a tracked number use the in-repo criterion benches (`BENCHMARKS.md`); this
  harness is for the cross-engine ratio, run it a few times and eyeball variance.
- **Hardware fingerprint matters.** Comparisons are only valid within one
  machine; don't compare a run here to a number measured elsewhere.

## Extending

The natural next workload is real **LUBM** (the literal Stage-1 gate). RDFox
ships `examples/data/{univ-bench.owl,lubm1.ttl}`; feeding the same data to
HornDB needs the `univ-bench.owl` TBox converted from OWL functional syntax to
N-Triples (e.g. via RDFox `export`), since `horndb-bench` reads N-Triples only.
That conversion step is the main missing piece and is left as a follow-up.
