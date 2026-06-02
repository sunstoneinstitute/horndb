#!/usr/bin/env python3
"""Generate synthetic N-Triples workloads for the RDFox comparison harness.

Two workloads, matching the two reasoning goals we benchmark:

  chain    N OUT.nt            a single-predicate path of N nodes — the input
                               to the transitive-closure comparison (SPEC-05).
  taxonomy DEPTH INST OUT.nt   an rdfs:subClassOf chain C0..C{DEPTH} plus INST
                               instances each typed at C0 — the input to the
                               OWL 2 RL materialization comparison (cax-sco +
                               scm-sco fire; Stage-1 LUBM gate is the same
                               class of workload).

Both emit canonical N-Triples so the identical file feeds HornDB (via
`horndb-bench`) and RDFox (via `import`). IRIs are stable and namespaced
under http://horndb.bench/ so the matching RDFox rule file lines up.
"""
import sys

NS = "http://horndb.bench/"
PRED = NS + "p"
RDF_TYPE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
RDFS_SUBCLASS = "http://www.w3.org/2000/01/rdf-schema#subClassOf"


def gen_chain(n: int, out: str) -> None:
    """N nodes n0..n{N-1}, edges n_i :p n_{i+1}. Closure = N*(N-1)/2 edges."""
    with open(out, "w") as f:
        for i in range(n - 1):
            f.write(f"<{NS}n{i}> <{PRED}> <{NS}n{i + 1}> .\n")
    print(f"chain: {n} nodes, {n - 1} edges, predicate <{PRED}> -> {out}", file=sys.stderr)


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
    if cmd == "chain":
        gen_chain(int(sys.argv[2]), sys.argv[3])
    elif cmd == "taxonomy":
        gen_taxonomy(int(sys.argv[2]), int(sys.argv[3]), sys.argv[4])
    elif cmd == "predicate":
        # Print the chain predicate IRI (so the shell never hardcodes it).
        print(PRED)
    else:
        print(f"unknown workload '{cmd}'", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
