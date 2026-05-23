#!/usr/bin/env bash
# Fetch the ORE 2015 corpus from Zenodo (record 18578) and extract
# only the ontologies named in harness/ore2015-selected.toml. The full
# 1,920-ontology corpus is too big to vendor; Stage-2 grows the
# selection beyond the hand-picked 10.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
DATA="$ROOT/crates/harness/data/ore2015"
mkdir -p "$DATA"

ZENODO_TARBALL="https://zenodo.org/record/18578/files/ore2015-corpus.tar.gz"

if [[ ! -f "$DATA/ore2015-corpus.tar.gz" ]]; then
    echo "fetching ORE 2015 corpus (large — ~3GB)…"
    curl -sSfL "$ZENODO_TARBALL" -o "$DATA/ore2015-corpus.tar.gz"
fi

# Extract only the paths named in ore2015-selected.toml.
SELECTED="$ROOT/harness/ore2015-selected.toml"
python3 -c '
import tomllib, sys
with open(sys.argv[1], "rb") as f:
    d = tomllib.load(f)
for o in d["ontologies"]:
    print(o["path"])
' "$SELECTED" | while read -r p; do
    if [[ ! -f "$DATA/$p" ]]; then
        echo "extracting $p"
        tar -xzf "$DATA/ore2015-corpus.tar.gz" -C "$DATA" "$p" || \
            echo "WARN: $p not found in tarball — update ore2015-selected.toml"
    fi
done

echo "done."
