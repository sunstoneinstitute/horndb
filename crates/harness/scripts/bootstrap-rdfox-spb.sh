#!/usr/bin/env bash
# Bring up an RDFox daemon with a persistent "spb" datastore loaded
# with the LDBC SPB ontology and OWL 2 RL reasoning enabled, ready
# for run-rdfox-spb-256.sh to hammer.
#
# Workflow:
#   Terminal 1:  ./crates/harness/scripts/bootstrap-rdfox-spb.sh
#                (blocks; daemon stays in the foreground so its log
#                 is visible. Ctrl+C when done with the run.)
#   Terminal 2:  ./crates/harness/scripts/run-rdfox-spb-256.sh
#
# Why a separate bootstrap script? RDFox's reasoning profile is set
# at datastore-creation time and the SPB ontology has to be loaded
# *before* the SPB driver starts pushing data. Doing this inside the
# run script would conflate "one-shot setup" with "repeatable measure-
# ment", and we want the measurement step to be re-runnable cheaply.
#
# RDFox vs SPB rule semantics: SPB ships a GraphDB-flavoured PIE
# ruleset (etc/owl2-rl.pie) which we do NOT use here. RDFox derives
# its own OWL 2 RL rules from the ontology axioms when the datastore
# is created with `par-complex-nn` and `set rule-domain user-and-owl`.
# If a SPB result on RDFox diverges from GraphDB, the *first* place
# to look is whether SPB depends on PIE-only extensions not in
# normative OWL 2 RL.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

RDFOX_BIN="${RDFOX_BIN:-RDFox}"
RDFOX_PORT="${RDFOX_PORT:-12110}"
RDFOX_DATASTORE="${RDFOX_DATASTORE:-spb}"
RDFOX_SERVER_DIR="${RDFOX_SERVER_DIR:-$HOME/.RDFox/server}"

# SPB v2.0 layout: ontologies live under datasets_and_queries/ontologies/
# in {core,domain,ldbc}/ subdirs. We import all .ttl files recursively
# but skip the ldbc/ subdir — its readme.txt explicitly says the zip
# there should NOT be unpacked because its contents have been split out
# into the SPARQL conformance fragments directory tree (a different
# load path that is not part of the standard SPB scenario).
SPB_ONTOLOGIES_DIR="${SPB_ONTOLOGIES_DIR:-$ROOT/crates/harness/data/ldbc-spb/datasets_and_queries/ontologies}"

if ! command -v "$RDFOX_BIN" >/dev/null 2>&1; then
    echo "error: RDFox binary '$RDFOX_BIN' not on PATH." >&2
    echo "       Install RDFox, or set RDFOX_BIN=/path/to/RDFox." >&2
    exit 2
fi

if [[ ! -d "$SPB_ONTOLOGIES_DIR" ]]; then
    cat >&2 <<EOF
error: SPB ontologies directory not found:
           $SPB_ONTOLOGIES_DIR

       Fetch LDBC SPB v2.0 first:
           $HERE/fetch-ldbc-spb.sh
       (which clones github.com/ldbc/ldbc_spb_bm_2.0 into
        $ROOT/crates/harness/data/ldbc-spb/ and tells you the exact
        ontologies path it found — set SPB_ONTOLOGIES_DIR from that
        message if it differs from this script's default.)

       Or set SPB_ONTOLOGIES_DIR=/path/to/your/spb/ontologies directly.
EOF
    exit 2
fi

# Recursive find, ldbc/ excluded (see comment on SPB_ONTOLOGIES_DIR
# above). NUL-delimited to survive any oddly-named files.
mapfile -d '' -t ontology_files < <(
    find "$SPB_ONTOLOGIES_DIR" -type f -name '*.ttl' -not -path '*/ldbc/*' -print0 | sort -z
)
if [[ ${#ontology_files[@]} -eq 0 ]]; then
    echo "error: no *.ttl files under $SPB_ONTOLOGIES_DIR (ldbc/ excluded)" >&2
    exit 2
fi

mkdir -p "$RDFOX_SERVER_DIR"

# Emit an RDFox shell init script that creates the datastore (if it
# doesn't already exist), activates it, and imports each ontology
# file. We use `dstore create -` with `if-not-exists` semantics by
# wrapping creation in a conditional via `dstore list | grep` outside
# RDFox -- RDFox's shell doesn't have idempotent-create natively, so
# we delete-and-recreate is the simplest re-runnable contract.
#
# NB: this wipes the datastore on every bootstrap. If you want to
# preserve a loaded SPB dataset across runs, set SPB_KEEP_DATASTORE=1.
INIT_SCRIPT="$(mktemp -t rdfox-spb-init.XXXXXX.rdfox)"
trap 'rm -f "$INIT_SCRIPT"' EXIT

{
    echo "set output out"
    echo "set endpoint.port $RDFOX_PORT"
    if [[ "${SPB_KEEP_DATASTORE:-0}" != "1" ]]; then
        echo "dstore delete $RDFOX_DATASTORE"
    fi
    # par-complex-nn: parallel reasoning, no named graphs (SPB is a
    # default-graph workload). The 'nn' variant is RDFox's standard
    # OWL 2 RL reasoning configuration.
    echo "dstore create $RDFOX_DATASTORE par-complex-nn"
    echo "active $RDFOX_DATASTORE"
    # Tell RDFox to translate supported OWL axioms in imported
    # ontologies into rules.
    echo "set rule-domain user-and-owl"
    for ttl in "${ontology_files[@]}"; do
        echo "import \"$ttl\""
    done
    echo "info datastore"
    echo "endpoint start"
} > "$INIT_SCRIPT"

cat <<EOF
Bootstrapping RDFox for SPB:
    binary       : $RDFOX_BIN
    server dir   : $RDFOX_SERVER_DIR
    datastore    : $RDFOX_DATASTORE
    endpoint     : http://127.0.0.1:$RDFOX_PORT/datastores/$RDFOX_DATASTORE/sparql
    ontology dir : $SPB_ONTOLOGIES_DIR
    ontologies   : ${#ontology_files[@]} TTL file(s)
    init script  : $INIT_SCRIPT

Launching daemon (Ctrl+C to stop). In another terminal:
    ./crates/harness/scripts/run-rdfox-spb-256.sh
EOF

# Foreground daemon. -p file = persistent file-backed store,
# -server-directory = where the persistence lives. `init -f` reads
# the shell commands from our generated script.
exec "$RDFOX_BIN" \
    -persistence file \
    -server-directory "$RDFOX_SERVER_DIR" \
    daemon \
    init -f "$INIT_SCRIPT"
