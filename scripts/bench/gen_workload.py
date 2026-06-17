#!/usr/bin/env python3
"""Generate synthetic N-Triples workloads for the RDFox comparison harness.

Two workloads, matching the two reasoning goals we benchmark:

  chain    N OUT.nt            a single-predicate path of N nodes — the input
            [--transitive]     to the transitive-closure comparison (SPEC-05).
  taxonomy DEPTH INST OUT.nt   an rdfs:subClassOf chain C0..C{DEPTH} plus INST
                               instances each typed at C0 — the input to the
                               OWL 2 RL materialization comparison (cax-sco +
                               scm-sco fire; Stage-1 LUBM gate is the same
                               class of workload).

Both emit canonical N-Triples so the identical file feeds HornDB (via
`horndb-bench`) and RDFox (via `import`). IRIs are stable and namespaced
under http://horndb.bench/ so the matching RDFox rule file lines up.

WHICH CONSUMER NEEDS WHAT:
  * `horndb-bench transitive` closes the predicate directly (GraphBLAS), so a
    bare `chain` is correct — no TBox needed.
  * `horndb-bench materialize` (OWL 2 RL) only closes the chain when the
    predicate is declared `owl:TransitiveProperty` (rule prp-trp). Feeding a
    bare `chain` into `materialize` infers ~nothing (just XSD datatype axioms)
    and makes the GraphBLAS closure backend look *slower* than rule-firing —
    because there is no closure to do. Pass `--transitive` to emit the
    declaration; that is the closure-dominated regime in BENCHMARKS.md's owlrl
    materialize A/B (the "transitive-property chain" row).
"""
import sys

NS = "http://horndb.bench/"
PRED = NS + "p"
RDF_TYPE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
RDFS_SUBCLASS = "http://www.w3.org/2000/01/rdf-schema#subClassOf"
OWL_TRANSITIVE = "http://www.w3.org/2002/07/owl#TransitiveProperty"


def gen_chain(n: int, out: str, transitive: bool = False) -> None:
    """N nodes n0..n{N-1}, edges n_i :p n_{i+1}. Closure = N*(N-1)/2 edges.

    When `transitive` is set, also declare the predicate an
    `owl:TransitiveProperty` so OWL 2 RL materialization (prp-trp) closes the
    chain; without it `horndb-bench materialize` has nothing to infer.
    """
    with open(out, "w") as f:
        for i in range(n - 1):
            f.write(f"<{NS}n{i}> <{PRED}> <{NS}n{i + 1}> .\n")
        if transitive:
            f.write(f"<{PRED}> <{RDF_TYPE}> <{OWL_TRANSITIVE}> .\n")
    decl = "; p = owl:TransitiveProperty" if transitive else ""
    print(
        f"chain: {n} nodes, {n - 1} edges, predicate <{PRED}>{decl} -> {out}",
        file=sys.stderr,
    )


def gen_taxonomy(depth: int, instances: int, out: str) -> None:
    """C0 subClassOf C1 ... subClassOf C{depth}; INST instances typed at C0.

    HornDB/RDFox both close this to: each instance typed at every C0..C{depth}
    (cax-sco), and the subClassOf chain made transitive (scm-sco).
    """
    with open(out, "w") as f:
        for i in range(depth):
            f.write(f"<{NS}C{i}> <{RDFS_SUBCLASS}> <{NS}C{i + 1}> .\n")
        for j in range(instances):
            f.write(f"<{NS}i{j}> <{RDF_TYPE}> <{NS}C0> .\n")
    base = depth + instances
    print(
        f"taxonomy: depth {depth}, {instances} instances, {base} base triples -> {out}",
        file=sys.stderr,
    )


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    cmd = sys.argv[1]
    # `--transitive` may appear anywhere after the subcommand; strip it out so
    # the remaining positional args keep the legacy `chain N OUT` interface the
    # shell drivers (compare-rdfox.sh) depend on.
    rest = sys.argv[2:]
    transitive = "--transitive" in rest
    rest = [a for a in rest if a != "--transitive"]
    if cmd == "chain":
        gen_chain(int(rest[0]), rest[1], transitive=transitive)
    elif cmd == "taxonomy":
        gen_taxonomy(int(rest[0]), int(rest[1]), rest[2])
    elif cmd == "predicate":
        # Print the chain predicate IRI (so the shell never hardcodes it).
        print(PRED)
    else:
        print(f"unknown workload '{cmd}'", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
