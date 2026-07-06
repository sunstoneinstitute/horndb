---
status: executed
date: 2026-06-03
scope: "Real LUBM-100 vs RDFox Materialization Comparison"
---

# Real LUBM-100 vs RDFox Materialization Comparison — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the synthetic taxonomy in the HornDB-vs-RDFox `materialize` comparison with a real LUBM-100 workload, with a generated matched ruleset and a hard closure-count parity gate, to measure the literal Stage-1 gate (within 3× of RDFox).

**Architecture:** A Python generator turns `crates/owlrl/rules.toml` into an RDFox Datalog ruleset (so both engines fire the same rules). A bash script drives the Lehigh UBA data generator to produce LUBM-N as N-Triples. A new `--lubm N` mode in `compare-rdfox.sh` runs both engines on identical TBox+ABox, enforces a closure-count parity gate, and caps HornDB's wall-clock so a slow nested-loop run records a finding instead of hanging.

**Tech Stack:** Python 3.11+ (`tomllib`, stdlib `unittest`), Bash, Apache Jena `riot`, RDFox sandbox CLI, Java 21 (UBA generator), the existing `horndb-bench` Rust runner (unchanged).

**Design doc:** `docs/specs/SPEC-14-lubm-rdfox-comparison.md`

---

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `scripts/bench/gen_ruleset.py` | Create | Translate `rules.toml` → RDFox Datalog `.dlog`. Pure, testable. |
| `scripts/bench/test_gen_ruleset.py` | Create | stdlib `unittest` tests for the generator. |
| `scripts/bench/gen_lubm.sh` | Create | Fetch UBA generator + RDF/XML ontology, generate LUBM-N, convert to N-Triples (`tbox.nt`, `abox.nt`). |
| `scripts/bench/compare-rdfox.sh` | Modify | Add `--lubm N` / `--cap-seconds`, `cap_run` helper, `lubm_compare`, parity gate. |
| `scripts/bench/README.md` | Modify | Document `--lubm`, Java requirement, disk footprint. |
| `.gitignore` | Modify | Ensure `scripts/bench/results/` is gitignored (RDFox numbers). |
| `docs/benchmarks.md` | Modify | Status-only note under Stage gates (no RDFox number). |
| `TASKS.md` + issue #10 | Modify | Reflect the wired LUBM A/B. |

**Key facts the engineer must not re-derive:**
- `rules.toml` has 49 `[[rule]]` blocks. Each has `id`, optional `comment`, optional `delegate`, `body = [ { s, p, o }, ... ]`, `head = { s, p, o }`. Terms are either variables (`"?x"`) or prefixed names (`"rdf:type"`, `"owl:Nothing"`).
- **Inconsistency rules** are exactly those whose `head.o == "owl:Nothing"` (cax-dw, cls-com, prp-irp, prp-asyp, prp-pdw, prp-npa1, prp-npa2, eq-diff1). They derive no normal triples — **omit** them.
- **`eq-ref`** has `body = []` (empty). It cannot be a Datalog rule and HornDB does not materialize reflexive `sameAs` as explicit triples — **omit** it (and any future empty-body rule).
- Everything else is **additive** and **included**, including `delegate = "closure"` rules (`eq-sym`, `eq-trans`, `scm-sco`, `scm-spo`, `prp-trp`) and the `sameAs`-deriving `prp-fp`/`prp-ifp`. RDFox computes these via native recursion.
- The target Datalog syntax is exactly what `scripts/bench/rules/owl2rl-core.dlog` already shows: `@prefix` lines, then `[s, p, o] :- [s,p,o], [s,p,o] .` Variables and prefixed names pass through verbatim.
- `horndb-bench materialize --data A --data B` already accepts multiple files and prints one-line JSON including `"total"` (closure size), `"asserted"`, `"inferred"`, `"reason_ms"`. **Do not modify the Rust runner.**
- HornDB injects a fixed XSD datatype-lattice base (~tens of axioms) so `HornDB total = RDFox total + XSD_OFFSET`. The exact offset is measured at N=1 in Task 5.
- RDFox numbers are **internal only** — never commit a measured RDFox number anywhere git tracks.

---

## Task 1: Ruleset generator (`gen_ruleset.py`)

**Files:**
- Create: `scripts/bench/gen_ruleset.py`
- Test: `scripts/bench/test_gen_ruleset.py`

- [ ] **Step 1: Write the failing test**

Create `scripts/bench/test_gen_ruleset.py`:

```python
"""Stdlib unittest for gen_ruleset.py — no external deps (matches repo style)."""
import io
import unittest
from pathlib import Path

import gen_ruleset as g

REPO_ROOT = Path(__file__).resolve().parents[2]
RULES_TOML = REPO_ROOT / "crates" / "owlrl" / "rules.toml"


class TermAndAtom(unittest.TestCase):
    def test_variable_passthrough(self):
        self.assertEqual(g.term("?x"), "?x")

    def test_prefixed_name_passthrough(self):
        self.assertEqual(g.term("rdf:type"), "rdf:type")

    def test_atom_format(self):
        a = g.atom({"s": "?x", "p": "rdf:type", "o": "owl:Thing"})
        self.assertEqual(a, "[?x, rdf:type, owl:Thing]")

    def test_rule_to_dlog_two_body(self):
        rule = {
            "id": "cax-sco",
            "body": [
                {"s": "?c1", "p": "rdfs:subClassOf", "o": "?c2"},
                {"s": "?x", "p": "rdf:type", "o": "?c1"},
            ],
            "head": {"s": "?x", "p": "rdf:type", "o": "?c2"},
        }
        self.assertEqual(
            g.rule_to_dlog(rule),
            "[?x, rdf:type, ?c2] :- [?c1, rdfs:subClassOf, ?c2], [?x, rdf:type, ?c1] .",
        )


class Classification(unittest.TestCase):
    def test_inconsistency_rule_excluded(self):
        rule = {"id": "cax-dw", "body": [{"s": "?x", "p": "rdf:type", "o": "?c1"}],
                "head": {"s": "?x", "p": "rdf:type", "o": "owl:Nothing"}}
        self.assertFalse(g.included(rule))

    def test_empty_body_rule_excluded(self):
        rule = {"id": "eq-ref", "body": [],
                "head": {"s": "?s", "p": "owl:sameAs", "o": "?s"}}
        self.assertFalse(g.included(rule))

    def test_sameas_deriving_rule_included(self):
        rule = {"id": "prp-fp",
                "body": [{"s": "?p", "p": "rdf:type", "o": "owl:FunctionalProperty"},
                         {"s": "?x", "p": "?p", "o": "?y1"},
                         {"s": "?x", "p": "?p", "o": "?y2"}],
                "head": {"s": "?y1", "p": "owl:sameAs", "o": "?y2"}}
        self.assertTrue(g.included(rule))


class FullGeneration(unittest.TestCase):
    def setUp(self):
        self.rules = g.load_rules(RULES_TOML)
        self.text = g.generate(self.rules)

    def test_includes_cax_sco(self):
        self.assertIn("rdfs:subClassOf", self.text)
        self.assertTrue(any(r["id"] == "cax-sco" for r in self.rules))

    def test_omits_inconsistency_and_lists_them(self):
        # owl:Nothing must never appear in an emitted rule body/head…
        for line in self.text.splitlines():
            if line.strip().startswith("["):
                self.assertNotIn("owl:Nothing", line)
        # …but the omitted rules must be named in a comment.
        self.assertIn("cax-dw", self.text)
        self.assertIn("OMITTED", self.text)

    def test_omits_eq_ref(self):
        for line in self.text.splitlines():
            if line.strip().startswith("[") and ":-" in line:
                # eq-ref's only signature would be a bodyless sameAs; ensure no
                # rule has an empty body (": - ." with nothing between).
                self.assertNotRegex(line, r":-\s*\.")

    def test_has_prefix_header(self):
        self.assertIn("@prefix rdf:", self.text)
        self.assertIn("@prefix owl:", self.text)


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd scripts/bench && python3 -m unittest test_gen_ruleset -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'gen_ruleset'`.

- [ ] **Step 3: Write the generator**

Create `scripts/bench/gen_ruleset.py`:

```python
#!/usr/bin/env python3
"""Generate an RDFox Datalog ruleset from HornDB's crates/owlrl/rules.toml.

The point: make RDFox fire exactly the rules HornDB's compiled engine fires,
so a HornDB-vs-RDFox materialization comparison is apples-to-apples. The
closure-count parity gate in compare-rdfox.sh validates the translation.

Inclusion policy (driven by each rule's head):
  * OMIT rules whose head asserts inconsistency (head.o == "owl:Nothing").
    They derive no normal triples, so they cannot affect the closure count.
  * OMIT rules with an empty body (eq-ref): not expressible as a Datalog rule,
    and HornDB does not materialize reflexive owl:sameAs as explicit triples.
  * INCLUDE everything else, including delegate="closure" rules (HornDB does
    these via GraphBLAS; RDFox via native recursion) and sameAs-deriving
    prp-fp/prp-ifp.

Usage:  gen_ruleset.py [--rules PATH]   # writes .dlog to stdout
"""
import argparse
import sys
from pathlib import Path

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover
    sys.exit("gen_ruleset.py requires Python 3.11+ (tomllib)")

PREFIXES = {
    "rdf": "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "rdfs": "http://www.w3.org/2000/01/rdf-schema#",
    "owl": "http://www.w3.org/2002/07/owl#",
    "xsd": "http://www.w3.org/2001/XMLSchema#",
}


def load_rules(path):
    with open(path, "rb") as fh:
        return tomllib.load(fh).get("rule", [])


def term(v):
    # Variables ("?x") and prefixed names ("rdf:type") both pass through to
    # RDFox Datalog verbatim, given the @prefix header below.
    return v


def atom(t):
    return f"[{term(t['s'])}, {term(t['p'])}, {term(t['o'])}]"


def is_inconsistency(rule):
    return rule.get("head", {}).get("o") == "owl:Nothing"


def is_empty_body(rule):
    return not rule.get("body")


def included(rule):
    return not is_empty_body(rule) and not is_inconsistency(rule)


def rule_to_dlog(rule):
    head = atom(rule["head"])
    body = ", ".join(atom(b) for b in rule["body"])
    return f"{head} :- {body} ."


def generate(rules):
    kept = [r for r in rules if included(r)]
    omitted = [r for r in rules if not included(r)]
    lines = []
    lines.append("# GENERATED by scripts/bench/gen_ruleset.py from crates/owlrl/rules.toml.")
    lines.append("# Do not edit by hand. RDFox runs this so it fires exactly HornDB's rules.")
    lines.append("# INTERNAL benchmarking artifact.")
    lines.append("#")
    lines.append("# OMITTED rules (head asserts inconsistency, or empty body — no derived triples):")
    for r in omitted:
        why = "inconsistency (owl:Nothing)" if is_inconsistency(r) else "empty body"
        lines.append(f"#   - {r['id']}: {why}")
    lines.append("")
    for pfx, iri in PREFIXES.items():
        lines.append(f"@prefix {pfx}: <{iri}> .")
    lines.append("")
    for r in kept:
        cid = r.get("id", "?")
        lines.append(f"# {cid}")
        lines.append(rule_to_dlog(r))
    return "\n".join(lines) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser()
    default = Path(__file__).resolve().parents[2] / "crates" / "owlrl" / "rules.toml"
    ap.add_argument("--rules", type=Path, default=default)
    args = ap.parse_args(argv)
    sys.stdout.write(generate(load_rules(args.rules)))


if __name__ == "__main__":
    main()
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd scripts/bench && python3 -m unittest test_gen_ruleset -v`
Expected: PASS (all tests OK).

- [ ] **Step 5: Eyeball the generated ruleset**

Run: `cd scripts/bench && python3 gen_ruleset.py | head -40`
Expected: an OMITTED comment block naming `cax-dw`, `cls-com`, `eq-ref`, etc.; `@prefix` lines; then rules like `[?x, rdf:type, ?c2] :- [?c1, rdfs:subClassOf, ?c2], [?x, rdf:type, ?c1] .` No emitted rule line contains `owl:Nothing`.

- [ ] **Step 6: Commit**

```bash
git add scripts/bench/gen_ruleset.py scripts/bench/test_gen_ruleset.py
git commit -F /dev/stdin <<'EOF'
feat(bench): generate matched RDFox ruleset from owlrl rules.toml

Translates HornDB's rules.toml into an RDFox Datalog ruleset so both
engines fire the same rules in the LUBM comparison. Omits inconsistency
rules (head owl:Nothing) and the empty-body eq-ref; includes additive
and delegate=closure rules. stdlib unittest, no new deps. Tracks #10.
EOF
```

---

## Task 2: LUBM data pipeline (`gen_lubm.sh`)

**Files:**
- Create: `scripts/bench/gen_lubm.sh`

This task fetches an external Java tool and writes large files under the gitignored `target/`. There is no unit test; the verification is a real N=1 run cross-checked against the RDFox-bundled `lubm1.ttl`.

- [ ] **Step 1: Write the script**

Create `scripts/bench/gen_lubm.sh`:

```bash
#!/usr/bin/env bash
#
# gen_lubm.sh — generate a LUBM-N workload as N-Triples for the RDFox comparison.
#
# Fetches the Lehigh UBA1.7 data generator (Java) and the canonical RDF/XML
# univ-bench.owl ontology, generates N universities, and converts everything to
# N-Triples via Apache Jena `riot`:
#
#   <out>/tbox.nt   the ontology (RDF/XML -> NT)
#   <out>/abox.nt   the generated instance data (RDF/XML -> NT, concatenated)
#
# All outputs live under the gitignored target/ tree.
#
# Usage: gen_lubm.sh --universities N [--out DIR] [--seed S]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

UNIV=1
SEED=0
OUTDIR_BASE="${OUTDIR:-$REPO_ROOT/target/bench-rdfox}"
OUT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --universities) UNIV="$2"; shift 2 ;;
    --seed)         SEED="$2"; shift 2 ;;
    --out)          OUT="$2"; shift 2 ;;
    -h|--help)      sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$OUT" ]] || OUT="$OUTDIR_BASE/lubm/$UNIV"

command -v java >/dev/null || { echo "java not found (UBA generator needs a JDK)" >&2; exit 1; }
command -v riot >/dev/null || { echo "riot not found (brew install jena)" >&2; exit 1; }

UBA_DIR="$OUTDIR_BASE/uba"
UBA_ZIP="$UBA_DIR/uba1.7.zip"
UBA_URL="http://swat.cse.lehigh.edu/projects/lubm/uba1.7.zip"
ONTO_URL="http://swat.cse.lehigh.edu/onto/univ-bench.owl"

mkdir -p "$UBA_DIR"
if [[ ! -f "$UBA_ZIP" ]]; then
  echo ">> fetching UBA1.7 generator" >&2
  curl -fsSL "$UBA_URL" -o "$UBA_ZIP"
fi
# Unzip once; the archive contains compiled classes + a bundled univ-bench.owl.
if [[ ! -d "$UBA_DIR/extracted" ]]; then
  mkdir -p "$UBA_DIR/extracted"
  unzip -o -q "$UBA_ZIP" -d "$UBA_DIR/extracted"
fi

# Locate the classpath root (dir containing edu/lehigh/.../Generator.class) and
# the bundled RDF/XML ontology.
GEN_CLASS="$(find "$UBA_DIR/extracted" -path '*edu/lehigh/swat/bench/uba/Generator.class' | head -1)"
[[ -n "$GEN_CLASS" ]] || { echo "Generator.class not found in UBA archive" >&2; exit 1; }
CP_ROOT="${GEN_CLASS%/edu/lehigh/swat/bench/uba/Generator.class}"
ONTO_SRC="$(find "$UBA_DIR/extracted" -iname 'univ-bench.owl' | head -1)"
[[ -n "$ONTO_SRC" ]] || { echo "bundled univ-bench.owl not found in UBA archive" >&2; exit 1; }

# Generate into a clean dir (UBA writes University*.owl into the CWD).
GENDIR="$OUT/generated"
rm -rf "$GENDIR"; mkdir -p "$GENDIR"
echo ">> generating LUBM-$UNIV (seed $SEED) into $GENDIR" >&2
( cd "$GENDIR" && java -cp "$CP_ROOT" edu.lehigh.swat.bench.uba.Generator \
    -univ "$UNIV" -index 0 -seed "$SEED" -onto "$ONTO_URL" >/dev/null )

# Convert ontology + all generated files to N-Triples.
echo ">> converting ontology -> tbox.nt" >&2
riot --syntax=RDFXML --output=NT "$ONTO_SRC" > "$OUT/tbox.nt"

echo ">> converting $(ls "$GENDIR"/University*.owl | wc -l | tr -d ' ') ABox files -> abox.nt" >&2
: > "$OUT/abox.nt"
for f in "$GENDIR"/University*.owl; do
  riot --syntax=RDFXML --output=NT "$f" >> "$OUT/abox.nt"
done

TBOX_N="$(wc -l < "$OUT/tbox.nt" | tr -d ' ')"
ABOX_N="$(wc -l < "$OUT/abox.nt" | tr -d ' ')"
echo ">> done: tbox.nt=$TBOX_N triples, abox.nt=$ABOX_N triples ($OUT)" >&2
# Sanity: the ontology must carry the structural axioms we reason over.
SCO="$(grep -c 'subClassOf' "$OUT/tbox.nt" || true)"
echo ">> tbox subClassOf triples: $SCO" >&2
[[ "$ABOX_N" -gt 0 ]] || { echo "abox.nt is empty — generation failed" >&2; exit 1; }
[[ "$SCO" -gt 0 ]] || { echo "tbox.nt has no subClassOf — wrong ontology variant?" >&2; exit 1; }
```

- [ ] **Step 2: Make it executable**

Run: `chmod +x scripts/bench/gen_lubm.sh`

- [ ] **Step 3: Run at N=1 and sanity-check against the bundled lubm1.ttl**

Run:
```bash
RH="$(dirname "$(find target/bench-rdfox/rdfox -name RDFox -type f | head -1)")"
scripts/bench/gen_lubm.sh --universities 1
echo "generated abox:"; wc -l target/bench-rdfox/lubm/1/abox.nt
echo "bundled lubm1: "; wc -l "$RH/examples/data/lubm1.ttl"
```
Expected: `gen_lubm.sh` prints non-zero `tbox.nt` (with subClassOf) and `abox.nt`. The generated `abox.nt` line count is the same order of magnitude as the bundled `lubm1.ttl` (~100k). Exact equality is NOT expected — different serialization / blank-node ids. If `abox.nt` is empty or `tbox.nt` has no subClassOf, stop and diagnose before proceeding.

- [ ] **Step 4: Commit**

```bash
git add scripts/bench/gen_lubm.sh
git commit -F /dev/stdin <<'EOF'
feat(bench): LUBM-N data pipeline via Lehigh UBA generator

Fetches UBA1.7 + the bundled RDF/XML univ-bench.owl, generates N
universities, and converts ontology and instance data to N-Triples
(tbox.nt / abox.nt) under the gitignored target tree. The RDF/XML
ontology variant resolves the OWL-functional-syntax conversion blocker.
Tracks #10.
EOF
```

---

## Task 3: `--lubm N` comparison mode in `compare-rdfox.sh`

**Files:**
- Modify: `scripts/bench/compare-rdfox.sh`

- [ ] **Step 1: Add config flags**

In the `# --- config ---` block (after `KEEP=0`, around line 36), add:

```bash
LUBM_N=0          # 0 = run the original three comparisons; >0 = LUBM mode
CAP_SECONDS=1800  # wall-clock cap for HornDB materialize (LUBM mode)
```

In the arg-parsing `while`/`case` (around line 42), add cases before the `*)` catch-all:

```bash
    --lubm)         LUBM_N="$2"; shift 2 ;;
    --cap-seconds)  CAP_SECONDS="$2"; shift 2 ;;
```

- [ ] **Step 2: Add the `cap_run` helper**

In the `# helpers` section (after the `run_rdfox` function, around line 83), add:

```bash
# cap_run <seconds> <outfile> <cmd...>
# Run a command writing stdout+stderr to <outfile>, killed after <seconds>.
# Returns 0 on success, 124 if it hit the cap (or otherwise failed). Portable:
# uses gtimeout/timeout if present, else a background-process + watchdog.
cap_run() {
  local cap="$1" out="$2"; shift 2
  if command -v timeout >/dev/null; then
    timeout "${cap}s" "$@" >"$out" 2>&1; return $?
  elif command -v gtimeout >/dev/null; then
    gtimeout "${cap}s" "$@" >"$out" 2>&1; return $?
  fi
  "$@" >"$out" 2>&1 &
  local pid=$!
  ( sleep "$cap"; kill -TERM "$pid" 2>/dev/null ) &
  local watcher=$!
  local rc=0
  wait "$pid" 2>/dev/null || rc=124
  kill "$watcher" 2>/dev/null || true
  wait "$watcher" 2>/dev/null || true
  return $rc
}
```

- [ ] **Step 3: Add the `lubm_compare` function**

Add after `materialize_compare()` (around line 203, before the call section):

```bash
# ----------------------------------------------------------------------------
# 4. LUBM-N OWL 2 RL materialization (Stage-1 gate: within 3x RDFox).
#    Same TBox + same ABox + SAME RULES (generated from rules.toml) through both
#    engines. Hard closure-count parity gate. HornDB capped at CAP_SECONDS.
# ----------------------------------------------------------------------------
lubm_compare() {
  local n="$1"
  local lubmdir="$OUTDIR/lubm/$n"
  if [[ ! -f "$lubmdir/abox.nt" || ! -f "$lubmdir/tbox.nt" ]]; then
    echo ">> [lubm] generating LUBM-$n data" >&2
    "$SCRIPT_DIR/gen_lubm.sh" --universities "$n" --out "$lubmdir" >&2
  fi

  # Matched ruleset, freshly generated from rules.toml each run (no drift).
  python3 "$SCRIPT_DIR/gen_ruleset.py" >"$WORK/owl2rl-horndb-subset.dlog"

  echo ">> [lubm] HornDB materialize (cap ${CAP_SECONDS}s)" >&2
  local capped=0 hj="" h_ms="—" h_total=""
  if cap_run "$CAP_SECONDS" "$LOGS/lubm.horndb.json" \
        "$HB" materialize --data "$lubmdir/tbox.nt" --data "$lubmdir/abox.nt"; then
    hj="$(cat "$LOGS/lubm.horndb.json")"
    h_ms="$(hb_field "$hj" reason_ms)"; h_total="$(hb_field "$hj" total)"
  else
    capped=1
    echo ">> [lubm] HornDB did not finish within ${CAP_SECONDS}s" >&2
  fi

  echo ">> [lubm] RDFox materialize (matched ruleset)" >&2
  cp "$lubmdir/tbox.nt" "$lubmdir/abox.nt" "$WORK/owl2rl-horndb-subset.dlog" "$WORK/"
  # Data first (tbox + abox), rules LAST -> last import time = reasoning only.
  run_rdfox "$WORK" 'dstore create default' 'import tbox.nt' 'import abox.nt' \
            'import owl2rl-horndb-subset.dlog' 'info' >"$LOGS/lubm.rdfox.log"
  local r_secs r_ms r_total
  r_secs="$(rdfox_import_secs "$LOGS/lubm.rdfox.log" last)"
  r_ms="$(awk -v s="$r_secs" 'BEGIN{printf "%.3f", s*1000}')"
  r_total="$(rdfox_facts "$LOGS/lubm.rdfox.log" all)"

  # --- closure-count parity gate ---
  # HornDB carries a fixed XSD datatype base, so HornDB total = RDFox + offset.
  # PASS iff 0 <= (h_total - r_total) <= XSD_OFFSET_MAX. HornDB having FEWER
  # facts means the ruleset translation dropped a rule -> hard FAIL.
  local XSD_OFFSET_MAX=512
  local parity ratio verdict
  if [[ "$capped" -eq 1 ]]; then
    parity="n/a (capped)"
    ratio="—"
    verdict="DID NOT COMPLETE within ${CAP_SECONDS}s"
  else
    local delta=$(( h_total - r_total ))
    if [[ "$delta" -ge 0 && "$delta" -le "$XSD_OFFSET_MAX" ]]; then
      parity="OK (HornDB +${delta})"
    else
      parity="MISMATCH (delta=${delta}) — ruleset translation suspect"
    fi
    ratio="$(fdiv "$h_ms" "$r_ms")"
    if [[ "$parity" == MISMATCH* ]]; then
      verdict="PARITY FAIL"
    else
      awk -v r="$ratio" 'BEGIN{exit !(r+0<=3.0)}' && verdict="PASS (within 3x)" || verdict="over 3x"
    fi
  fi

  ROWS+=("lubm-$n|reason ms|${h_ms}|${r_ms}|${ratio}x|within 3x|$verdict")
  DETAIL+=("lubm-$n     : LUBM($n); closure ${h_total:-—} / ${r_total} facts; parity ${parity}")
}
```

- [ ] **Step 4: Gate the call section on the mode**

Replace the call section (lines 206-208):

```bash
import_compare
transitive_compare
materialize_compare
```

with:

```bash
if [[ "$LUBM_N" -gt 0 ]]; then
  lubm_compare "$LUBM_N"
else
  import_compare
  transitive_compare
  materialize_compare
fi
```

- [ ] **Step 5: Verify the script still parses and `--help` works**

Run: `bash -n scripts/bench/compare-rdfox.sh && echo "syntax OK"`
Expected: `syntax OK` (no parse errors).

Run: `scripts/bench/compare-rdfox.sh --help | head -5`
Expected: the usage header prints (no crash).

- [ ] **Step 6: Commit**

```bash
git add scripts/bench/compare-rdfox.sh
git commit -F /dev/stdin <<'EOF'
feat(bench): add --lubm N comparison mode with parity gate

Runs HornDB and RDFox over identical LUBM-N TBox+ABox using a ruleset
generated from rules.toml, with a hard closure-count parity gate and a
--cap-seconds wall-clock cap. A capped HornDB run records a "did not
complete" finding rather than hanging. Tracks #10.
EOF
```

---

## Task 4: gitignore + README

**Files:**
- Modify: `.gitignore`
- Modify: `scripts/bench/README.md`

- [ ] **Step 1: Verify whether `scripts/bench/results/` is gitignored**

Run: `git check-ignore scripts/bench/results || echo "NOT IGNORED"`
Expected: prints `NOT IGNORED` (only `/target` is currently ignored).

- [ ] **Step 2: Add the ignore rule**

Append to `.gitignore`:

```
# Internal RDFox comparison results — must never be committed (DeWitt clause).
/scripts/bench/results/
```

- [ ] **Step 3: Verify it now ignores**

Run: `git check-ignore scripts/bench/results && echo "IGNORED OK"`
Expected: prints the path then `IGNORED OK`.

- [ ] **Step 4: Document the new mode in the README**

In `scripts/bench/README.md`, under the "Extending" section (or a new "LUBM mode" subsection), add:

```markdown
## LUBM-N mode (`--lubm N`)

Measures the literal Stage-1 gate — *LUBM-100 materialization within 3× of
RDFox* — on real LUBM data, with both engines firing the **same** rules.

```bash
# Wire-up smoke test on LUBM(1) (~100k triples)
scripts/bench/compare-rdfox.sh --lubm 1

# The Stage-1 gate workload (~13M triples). Slow on HornDB's nested-loop
# backend; capped at 30 min by default (override with --cap-seconds).
scripts/bench/compare-rdfox.sh --lubm 100
```

How it works:
- `gen_lubm.sh` fetches the Lehigh **UBA1.7** generator (needs a **JDK** — Java
  21 is fine) and converts the generated RDF/XML to N-Triples with `riot`.
  LUBM-100 produces a multi-GB `abox.nt` under the gitignored `target/` tree.
- `gen_ruleset.py` regenerates the RDFox ruleset from `crates/owlrl/rules.toml`
  on every run, so RDFox fires exactly HornDB's rules (no drift, no hand-copy).
- A **closure-count parity gate** asserts HornDB and RDFox derive the same facts
  (within HornDB's fixed XSD-base offset). A mismatch fails the run — it means
  the ruleset translation dropped or added a rule.
- HornDB's `materialize` uses the nested-loop `RuleFiringBackend`; if it exceeds
  `--cap-seconds`, the run records **"did not complete"** as a valid Stage-1
  finding rather than hanging.

RDFox numbers stay **internal only** (gitignored `scripts/bench/results/`).
```

- [ ] **Step 5: Commit**

```bash
git add .gitignore scripts/bench/README.md
git commit -F /dev/stdin <<'EOF'
docs(bench): gitignore results dir and document --lubm mode

Ensures scripts/bench/results/ (RDFox numbers) is gitignored and
documents the LUBM-N comparison mode, the JDK/riot requirements, the
parity gate, and the wall-clock cap. Tracks #10.
EOF
```

---

## Task 5: End-to-end validation (N=1, then N=100)

**Files:** none (verification + recorded results under gitignored `scripts/bench/results/`).

- [ ] **Step 1: Resolve RDFox env and run the N=1 comparison**

Run:
```bash
export RDFOX_HOME="$(dirname "$(find target/bench-rdfox/rdfox -name RDFox -type f | head -1)")"
scripts/bench/compare-rdfox.sh --lubm 1 --keep
```
Expected: the report prints a `lubm-1` row. The DETAIL line shows `parity OK (HornDB +<delta>)`. Record the `<delta>` — this is the XSD-base offset.

- [ ] **Step 2: Confirm the parity gate is meaningful**

The parity delta must be a small, stable, non-negative number (HornDB's XSD base, expected well under a few hundred). If the gate reports `MISMATCH` or `PARITY FAIL`:
- Inspect `target/bench-rdfox/logs/lubm.rdfox.log` (RDFox fact count) vs `target/bench-rdfox/logs/lubm.horndb.json` (`total`).
- Re-read `scripts/bench/work/owl2rl-horndb-subset.dlog` against `rules.toml` — a missing additive rule makes HornDB derive *more* (delta too big); an extra/over-firing rule makes RDFox derive more (delta negative).
- Fix `gen_ruleset.py`, re-run Task 1 tests, re-run this step. **Do not proceed until parity is OK.**

- [ ] **Step 3: Tighten the offset constant (optional)**

If the measured `<delta>` is, say, 60, the `XSD_OFFSET_MAX=512` ceiling in `lubm_compare` is comfortably generous — leave it. Only narrow it if the observed delta is surprisingly large and you want a tighter guard. If you change it, re-commit `compare-rdfox.sh` with a one-line message noting the measured offset.

- [ ] **Step 4: Run the Stage-1 workload (N=100)**

Run:
```bash
scripts/bench/compare-rdfox.sh --lubm 100
```
Expected: a recorded outcome — either a `lubm-100` row with a ratio and `PASS/over 3x`, **or** `DID NOT COMPLETE within 1800s`. Both are valid Stage-1 findings. Confirm results landed under `scripts/bench/results/run-*/` and that `git check-ignore` covers them.

- [ ] **Step 5: Record the finding (internal)**

Note the N=100 outcome (ratio or "did not complete") for the Task 6 status update. **Do not write any RDFox number into a tracked file.** The status update is qualitative only.

---

## Task 6: Sync docs/benchmarks.md / TASKS.md / issue #10

**Files:**
- Modify: `docs/benchmarks.md`
- Modify: `TASKS.md`
- GitHub: issue #10

- [ ] **Step 1: Add a status-only note under the Stage gates table**

In `docs/benchmarks.md`, immediately after the Stage gates table (after line 41), add:

```markdown
> **Stage-1 LUBM gate — measurement status (internal):** wired and runnable via
> `scripts/bench/compare-rdfox.sh --lubm 100`. Both engines fire the same rules
> (RDFox ruleset generated from `crates/owlrl/rules.toml`) over identical LUBM
> TBox+ABox, with a closure-count parity gate. RDFox comparison numbers are
> internal only (DeWitt clause) and are never recorded here.
```

(Do **not** edit the `within 3×` target or add any measured number.)

- [ ] **Step 2: Update the tracking task in TASKS.md**

Find the task line for issue #10:

Run: `grep -n '#10' TASKS.md`

The current line reads:

```
- [ ] **MEDIUM** · _Conformance_ — SPEC-01 harness (full W3C/ORE/LDBC/LUBM suites, RDFox A/B) ([#10](https://github.com/sunstoneinstitute/horndb/issues/10))
```

Replace it with (LUBM materialization A/B now wired; full-suite coverage still open, so it stays unchecked):

```
- [ ] **MEDIUM** · _Conformance_ — SPEC-01 harness (full W3C/ORE/LDBC suites; LUBM materialization RDFox A/B wired via `scripts/bench/compare-rdfox.sh --lubm`, full-suite coverage outstanding) ([#10](https://github.com/sunstoneinstitute/horndb/issues/10))
```

If issue #10 also has a body heading in TASKS.md (search the file body, not just the index line), apply the same wording there. Per CLAUDE.md, if this flips a Status field in `docs/architecture.md`, update it in the same commit; if nothing moves, leave `docs/architecture.md` untouched (drop it from the `git add` in Step 5).

- [ ] **Step 3: Verify the docs are internally consistent**

Run: `grep -n 'lubm\|LUBM\|--lubm' docs/benchmarks.md scripts/bench/README.md TASKS.md`
Expected: the LUBM mode is referenced consistently across all three; no RDFox number appears anywhere.

- [ ] **Step 4: Update the GitHub issue to match**

Run:
```bash
gh issue comment 10 --body "LUBM materialization A/B now wired via \`scripts/bench/compare-rdfox.sh --lubm N\`: generated matched ruleset from rules.toml, closure-count parity gate, wall-clock cap. Full W3C/ORE/LDBC suite coverage remains open under this issue."
```
(Do not close #10 — full-suite coverage is still outstanding. Do not paste RDFox numbers.)

- [ ] **Step 5: Commit**

```bash
git add docs/benchmarks.md TASKS.md docs/architecture.md
git commit -F /dev/stdin <<'EOF'
docs: record LUBM A/B wiring status across BENCHMARKS/TASKS

Status-only update: the Stage-1 LUBM materialization A/B is wired and
runnable (--lubm N) with a generated matched ruleset and a parity gate.
No RDFox numbers recorded (DeWitt clause). Tracks #10.
EOF
```

- [ ] **Step 6: Final gate — run the pre-push checks**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo build --workspace`
Expected: all green. (No Rust changed, but the pre-push gate must pass before pushing.)

---

## Self-Review Notes

- **Spec coverage:** §What-we-measure → Tasks 3/5; §Component 1 (ruleset gen) → Task 1; §Component 2 (data pipeline) → Task 2; §Component 3 (harness mode) → Task 3; §Risks (cap, Java, disk) → Tasks 2/3/4; §Acceptance #1–5 → Tasks 1/2/3/5/6. Licence constraint → Tasks 4/5/6 (gitignore + status-only).
- **Type/name consistency:** `gen_ruleset.py` public names used by tests and the harness: `term`, `atom`, `included`, `rule_to_dlog`, `load_rules`, `generate`. `compare-rdfox.sh` adds `LUBM_N`, `CAP_SECONDS`, `cap_run`, `lubm_compare`, reuses existing `hb_field`, `rdfox_import_secs`, `rdfox_facts`, `fdiv`, `run_rdfox`, `OUTDIR`, `WORK`, `LOGS`, `HB`, `SCRIPT_DIR`. `gen_lubm.sh` flags `--universities/--seed/--out`, outputs `tbox.nt`/`abox.nt` — consumed verbatim by `lubm_compare`.
- **Parity-gate direction:** HornDB ≥ RDFox by the XSD offset; HornDB fewer ⇒ dropped rule ⇒ FAIL. Validated at N=1 (Task 5) before trusting N=100.
