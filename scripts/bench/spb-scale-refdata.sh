#!/usr/bin/env bash
# Append the SPB reference datasets (DBpedia/GeoNames entities), closed under the
# SPB ontologies, to an SPB closure file built by spb-scale-build.sh.
#
# Why: at start-up the LDBC driver fills its entity lists by querying the store
# (`analyzereferencedata.txt`: `dbo:Company`/`dbo:Event`/`foaf:Person` with
# `ldbcspb:hasRDFRank`). A corpus without the reference data returns no entities,
# so every editorial INSERT throws inside the driver before any request is sent —
# `editorial-insert-count` 0 on every engine from HDB-37 on.
#
# Idempotent: a marker file next to the closure records that the append happened.
# After it runs, reload GraphDB and Oxigraph (spb-scale-bootstrap-engines.sh
# LEGS="G O"); HornDB serves the file directly and needs nothing.
#
# Knobs: SPB_SF (default sf128), SPB_WORK, SPB_ASSETS.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

DIST="${SPB_ASSETS:-/home/bench/src/horndb/crates/harness/data/ldbc-spb/dist}"
SF="${SPB_SF:-sf128}"
WORK="${SPB_WORK:-/home/bench/horndb-bench/spb-$SF}"
FINAL="$WORK/spb-$SF.nt"
MARK="$FINAL.refdata"

test -s "$FINAL" || { echo "spb-scale-refdata: no closure at $FINAL" >&2; exit 1; }
if [ -e "$MARK" ]; then echo "spb-scale-refdata: already appended ($MARK)"; exit 0; fi

mapfile -t ONTO < <(find "$DIST/data/ontologies" -name '*.ttl' -not -path '*/ldbc/*' | sort)
mapfile -t REFDATA < <(find "$DIST/data/datasets" -type f \( -name '*.ttl' -o -name '*.nt' \) | sort)
[ "${#REFDATA[@]}" -gt 0 ] || { echo "spb-scale-refdata: no reference data under $DIST/data/datasets" >&2; exit 1; }

cargo build -q --release -p horndb-bench-rdfox
OUT="$WORK/refdata-closure.nt"
./target/release/horndb-bench materialize --dump-nt "$OUT" --data "${ONTO[@]}" "${REFDATA[@]}"
echo "spb-scale-refdata: appending $(wc -l < "$OUT") lines to $FINAL"
cat "$OUT" >> "$FINAL"
rm -f "$OUT"
touch "$MARK"
