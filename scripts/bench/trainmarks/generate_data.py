"""
Generate synthetic RDF benchmark data in Turtle and N-Triples format.
Three scales: medium (100K triples), large (1M triples), and xlarge (10M triples).

Domain: e-commerce — customers, orders, products (same domain as the maplib masterclass).
"""

import random
import os
import time

random.seed(42)

COUNTRIES = ["Norway", "Sweden", "Denmark", "Finland", "Germany", "France", "UK", "USA", "Canada", "Japan"]
SEGMENTS = ["Enterprise", "SMB", "Startup", "Consumer", "Government"]
CATEGORIES = ["Software", "Hardware", "Services", "Accessories", "Support"]
STATUSES = ["completed", "pending", "shipped", "cancelled", "returned"]
PRIORITIES = ["low", "medium", "high", "critical"]
FIRST_NAMES = ["Alice", "Bob", "Charlie", "Diana", "Eve", "Frank", "Grace", "Hank", "Ivy", "Jack",
               "Karen", "Leo", "Mona", "Nate", "Olivia", "Paul", "Quinn", "Rosa", "Sam", "Tina"]
LAST_NAMES = ["Hansen", "Berg", "Larsen", "Olsen", "Johansen", "Andersen", "Pedersen", "Nilsen",
              "Eriksen", "Kristiansen", "Dahl", "Bakke", "Moe", "Vik", "Lund"]
PRODUCT_NAMES = ["Laptop Pro", "GPU Cluster", "Cloud License", "USB Hub", "Monitor 4K",
                 "Keyboard MX", "Mouse Ergo", "SSD 2TB", "RAM Kit", "Server Rack",
                 "VPN Service", "Support Plan", "Tablet X", "Webcam HD", "Dock Station"]

PREFIXES_TTL = """@prefix : <http://benchmark.example/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .

"""


def generate_triples(n_customers, n_products, n_orders):
    """Generate triple data as (subject, predicate, object) tuples."""
    triples = []

    # Customers
    for i in range(n_customers):
        cid = f":C{i:06d}"
        name = f"{random.choice(FIRST_NAMES)} {random.choice(LAST_NAMES)}"
        triples.append((cid, "rdf:type", ":Customer"))
        triples.append((cid, "rdfs:label", f'"{name}"'))
        triples.append((cid, ":email", f'"{name.lower().replace(" ", ".")}@example.com"'))
        triples.append((cid, ":country", f':{random.choice(COUNTRIES)}'))
        triples.append((cid, ":segment", f':{random.choice(SEGMENTS)}'))
        triples.append((cid, ":signupDate", f'"{2020 + random.randint(0,5)}-{random.randint(1,12):02d}-{random.randint(1,28):02d}"^^xsd:date'))

    # Products
    for i in range(n_products):
        pid = f":P{i:06d}"
        name = random.choice(PRODUCT_NAMES) if i < len(PRODUCT_NAMES) else f"Product-{i}"
        price = round(random.uniform(29.99, 2999.99), 2)
        triples.append((pid, "rdf:type", ":Product"))
        triples.append((pid, "rdfs:label", f'"{name}"'))
        triples.append((pid, ":unitPrice", f'"{price}"^^xsd:double'))
        triples.append((pid, ":category", f'"{random.choice(CATEGORIES)}"'))
        triples.append((pid, ":stockLevel", f'"{random.randint(0, 500)}"^^xsd:integer'))

    # Orders (bulk of the triples)
    for i in range(n_orders):
        oid = f":ORD{i:07d}"
        cust = f":C{random.randint(0, n_customers-1):06d}"
        prod = f":P{random.randint(0, n_products-1):06d}"
        qty = random.randint(1, 20)
        amount = round(random.uniform(50, 5000), 2)
        triples.append((oid, "rdf:type", ":Order"))
        triples.append((oid, ":placedBy", cust))
        triples.append((oid, ":contains", prod))
        triples.append((oid, ":quantity", f'"{qty}"^^xsd:integer'))
        triples.append((oid, ":totalAmount", f'"{amount}"^^xsd:double'))
        triples.append((oid, ":orderDate", f'"{2021 + random.randint(0,4)}-{random.randint(1,12):02d}-{random.randint(1,28):02d}"^^xsd:date'))
        triples.append((oid, ":orderStatus", f'"{random.choice(STATUSES)}"'))

    return triples


def write_turtle(triples, filepath):
    """Write triples as Turtle."""
    with open(filepath, "w") as f:
        f.write(PREFIXES_TTL)
        for s, p, o in triples:
            f.write(f"{s} {p} {o} .\n")


# ── N-Triples expansion ──
NS = "http://benchmark.example/"
RDF_NS = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
RDFS_NS = "http://www.w3.org/2000/01/rdf-schema#"
XSD_NS = "http://www.w3.org/2001/XMLSchema#"
SKOS_NS = "http://www.w3.org/2004/02/skos/core#"
PREFIX_MAP = [
    ("rdfs:", RDFS_NS),
    ("rdf:", RDF_NS),
    ("xsd:", XSD_NS),
    ("skos:", SKOS_NS),
    (":", NS),
]

def expand(term):
    if term.startswith('"'):
        if "^^xsd:" in term:
            val, dtype = term.rsplit("^^xsd:", 1)
            return f'{val}^^<{XSD_NS}{dtype}>'
        return term
    for prefix, full in PREFIX_MAP:
        if term.startswith(prefix):
            return f"<{full}{term[len(prefix):]}>"
    return term

def expand_object(o):
    if o.startswith('"') and "^^xsd:" in o:
        val_part, type_part = o.rsplit("^^xsd:", 1)
        return f'{val_part}^^<{XSD_NS}{type_part}>'
    elif o.startswith('"'):
        return o
    else:
        return expand(o)


def write_ntriples(triples, filepath):
    """Write triples as N-Triples (full IRIs, no prefixes)."""
    with open(filepath, "w") as f:
        for s, p, o in triples:
            f.write(f"{expand(s)} {expand(p)} {expand_object(o)} .\n")


def generate_sparql_queries():
    """Generate the benchmark SPARQL queries."""
    queries = {}

    queries["q1_count"] = """SELECT (COUNT(*) AS ?count) WHERE { ?s ?p ?o . }"""

    queries["q2_customer_orders"] = """
PREFIX : <http://benchmark.example/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

SELECT ?customer_name (COUNT(?order) AS ?order_count) (SUM(?amount) AS ?total_spend)
WHERE {
    ?order :placedBy ?customer ;
           :totalAmount ?amount .
    ?customer rdfs:label ?customer_name .
}
GROUP BY ?customer_name
ORDER BY DESC(?total_spend)
LIMIT 20
"""

    queries["q3_join_3_entities"] = """
PREFIX : <http://benchmark.example/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

SELECT ?customer_name ?product_name ?amount ?status
WHERE {
    ?order :placedBy ?customer ;
           :contains ?product ;
           :totalAmount ?amount ;
           :orderStatus ?status .
    ?customer rdfs:label ?customer_name ;
              :country :Norway .
    ?product rdfs:label ?product_name .
}
ORDER BY DESC(?amount)
LIMIT 50
"""

    queries["q4_optional_aggregation"] = """
PREFIX : <http://benchmark.example/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

SELECT ?country ?segment
       (COUNT(DISTINCT ?customer) AS ?customers)
       (COUNT(DISTINCT ?order) AS ?orders)
       (SUM(?amount) AS ?revenue)
WHERE {
    ?customer rdf:type :Customer ;
              :country ?country ;
              :segment ?segment .
    OPTIONAL {
        ?order :placedBy ?customer ;
               :totalAmount ?amount .
    }
}
GROUP BY ?country ?segment
ORDER BY DESC(?revenue)
"""

    queries["q5_construct"] = """
PREFIX : <http://benchmark.example/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

CONSTRUCT {
    ?customer rdf:type :Customer .
    ?customer rdfs:label ?name .
    ?customer :country :Norway .
    ?order :placedBy ?customer .
    ?order :totalAmount ?amount .
    ?order :contains ?product .
    ?product rdfs:label ?productName .
}
WHERE {
    ?customer rdf:type :Customer ;
              rdfs:label ?name ;
              :country :Norway .
    ?order :placedBy ?customer ;
           :totalAmount ?amount ;
           :contains ?product .
    ?product rdfs:label ?productName .
}
"""

    queries["q6_delete_insert"] = """
PREFIX : <http://benchmark.example/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>

DELETE { ?product :unitPrice ?oldPrice }
INSERT { ?product :unitPrice ?newPrice }
WHERE {
    ?product a :Product ;
             :unitPrice ?oldPrice ;
             :category ?cat .
    BIND(
        IF(?cat = "Software", ?oldPrice * 1.10,
        IF(?cat = "Hardware", ?oldPrice * 0.95,
        IF(?cat = "Services", ?oldPrice * 1.15,
        IF(?cat = "Accessories", ?oldPrice * 0.90,
        ?oldPrice * 1.05))))
        AS ?newPrice
    )
}
"""

    return queries


if __name__ == "__main__":
    os.makedirs("data", exist_ok=True)
    os.makedirs("queries", exist_ok=True)

    # --- Medium: ~100K triples ---
    print("Generating medium dataset (~100K triples)...")
    # 1000 customers * 6 = 6K, 200 products * 5 = 1K, ~13K orders * 7 = ~91K => ~98K
    t0 = time.time()
    triples_medium = generate_triples(n_customers=1000, n_products=200, n_orders=13000)
    print(f"  Generated {len(triples_medium)} triples in {time.time()-t0:.2f}s")

    t0 = time.time()
    write_turtle(triples_medium, "data/medium.ttl")
    print(f"  Wrote medium.ttl in {time.time()-t0:.2f}s")

    t0 = time.time()
    write_ntriples(triples_medium, "data/medium.nt")
    print(f"  Wrote medium.nt in {time.time()-t0:.2f}s")

    # --- Large: ~1M triples ---
    print("\nGenerating large dataset (~1M triples)...")
    # 10K customers * 6 = 60K, 2K products * 5 = 10K, ~133K orders * 7 = ~931K => ~1M
    t0 = time.time()
    triples_large = generate_triples(n_customers=10000, n_products=2000, n_orders=133000)
    print(f"  Generated {len(triples_large)} triples in {time.time()-t0:.2f}s")

    t0 = time.time()
    write_turtle(triples_large, "data/large.ttl")
    print(f"  Wrote large.ttl in {time.time()-t0:.2f}s")

    t0 = time.time()
    write_ntriples(triples_large, "data/large.nt")
    print(f"  Wrote large.nt in {time.time()-t0:.2f}s")

    # --- XLarge: ~10M triples ---
    print("\nGenerating xlarge dataset (~10M triples)...")
    # 100K customers * 6 = 600K, 10K products * 5 = 50K, ~1.335M orders * 7 = ~9.35M => ~10M
    t0 = time.time()
    triples_xlarge = generate_triples(n_customers=100000, n_products=10000, n_orders=1335000)
    print(f"  Generated {len(triples_xlarge)} triples in {time.time()-t0:.2f}s")

    t0 = time.time()
    write_turtle(triples_xlarge, "data/xlarge.ttl")
    print(f"  Wrote xlarge.ttl in {time.time()-t0:.2f}s")

    t0 = time.time()
    write_ntriples(triples_xlarge, "data/xlarge.nt")
    print(f"  Wrote xlarge.nt in {time.time()-t0:.2f}s")

    # --- SPARQL queries ---
    queries = generate_sparql_queries()
    for name, query in queries.items():
        with open(f"queries/{name}.rq", "w") as f:
            f.write(query.strip() + "\n")
    print(f"\nWrote {len(queries)} SPARQL queries to queries/")

    # Print file sizes
    print("\nFile sizes:")
    for f in ["data/medium.ttl", "data/medium.nt", "data/large.ttl", "data/large.nt",
              "data/xlarge.ttl", "data/xlarge.nt"]:
        size_mb = os.path.getsize(f) / 1024 / 1024
        print(f"  {f}: {size_mb:.1f} MB")
