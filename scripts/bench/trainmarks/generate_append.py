"""Generate an *append* corpus for the incremental bulk-load measurement (HDB-91).

Not vendored from upstream trainmarks — ours. It produces a second corpus to
load into a store that already holds `xlarge.nt`, in two vocabulary flavours
that differ *only* in whether the terms are already in the dictionary:

  --mode overlap  every subject is a new order IRI; every other term
                  (predicates, customers, products, the `Order` class, and all
                  literal values) is one `xlarge.nt` already contains.
                  Dictionary misses = one per order.

  --mode fresh    identical shape, identical predicates, but the entity IRIs
                  live in a second namespace and the literal values are drawn
                  from disjoint ranges, so almost every term is new.
                  Dictionary misses = three per order plus the literal grid.

Predicates are the same in both modes on purpose: they decide which tier
partitions the append lands in, so holding them fixed keeps the tier work
comparable and leaves vocabulary hit rate as the only variable.

Six triples per order, so `--triples` must be a multiple of 6.

    python3 generate_append.py --mode overlap --triples 1002000 \
        --out data/append_overlap.nt
"""

import argparse

NS = "http://benchmark.example/"
NS2 = "http://benchmark2.example/"
RDF_TYPE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
XSD = "http://www.w3.org/2001/XMLSchema#"

# `xlarge.nt` entity counts (generate_data.py: n_customers=100000,
# n_products=10000, n_orders=1335000).
BASE_CUSTOMERS = 100_000
BASE_PRODUCTS = 10_000
BASE_ORDERS = 1_335_000
STATUSES = ["completed", "pending", "shipped", "cancelled", "returned"]


def emit(mode, orders, out):
    fresh = mode == "fresh"
    ns = NS2 if fresh else NS
    # Order IRIs are new in both modes: an appended triple has to be new, and
    # its subject is the cheapest place to put the novelty.
    order_base = BASE_ORDERS + 1_000_000
    statuses = [f"b2-{s}" for s in STATUSES] if fresh else STATUSES
    year0 = 3021 if fresh else 2021
    qty0 = 1000 if fresh else 1
    order_cls = f"{ns}Order"
    with open(out, "w") as f:
        for i in range(orders):
            s = f"<{ns}ORD{order_base + i:07d}>"
            if fresh:
                # One new customer and one new product per order: a corpus that
                # brings its own entities, as a genuinely new dataset does.
                cust = f"<{NS2}C{i:07d}>"
                prod = f"<{NS2}P{i:07d}>"
            else:
                cust = f"<{NS}C{i % BASE_CUSTOMERS:06d}>"
                prod = f"<{NS}P{i % BASE_PRODUCTS:06d}>"
            qty = qty0 + i % 20
            date = f"{year0 + i % 5}-{1 + i % 12:02d}-{1 + i % 28:02d}"
            status = statuses[i % len(statuses)]
            f.write(f"{s} <{RDF_TYPE}> <{order_cls}> .\n")
            f.write(f"{s} <{NS}placedBy> {cust} .\n")
            f.write(f"{s} <{NS}contains> {prod} .\n")
            f.write(f'{s} <{NS}quantity> "{qty}"^^<{XSD}integer> .\n')
            f.write(f'{s} <{NS}orderDate> "{date}"^^<{XSD}date> .\n')
            f.write(f'{s} <{NS}orderStatus> "{status}" .\n')


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--mode", choices=["overlap", "fresh"], required=True)
    ap.add_argument("--triples", type=int, default=1_002_000)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    if args.triples % 6:
        raise SystemExit("--triples must be a multiple of 6 (six triples per order)")
    orders = args.triples // 6
    emit(args.mode, orders, args.out)
    print(f"{args.out}: {args.triples} triples ({orders} orders, mode={args.mode})")


if __name__ == "__main__":
    main()
