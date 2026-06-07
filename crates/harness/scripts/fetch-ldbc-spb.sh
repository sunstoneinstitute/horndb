#!/usr/bin/env bash
# Fetch the LDBC Semantic Publishing Benchmark v2.0 source tree into
# crates/harness/data/ldbc-spb/, which is where the bootstrap and run
# scripts for both engines (HornDB, GraphDB Free, RDFox) look for the
# ontologies and the driver JAR.
#
# What this script does:
#   1. git-clones github.com/ldbc/ldbc_spb_bm_2.0 (Apache 2.0).
#   2. Probes the working tree for the BBC core ontologies and prints
#      whichever path it actually found, so the sibling bootstrap
#      scripts can be pointed at it.
#   3. Optionally builds the driver JAR via Ant (set SPB_BUILD_DRIVER=1).
#      SPB v2.0 is an Ant project (build.xml, no pom.xml); building
#      requires a JDK and Ant. Skipped by default because not every
#      workstation has them installed and the bootstrap step is also
#      useful without a built JAR.
#
# What this script does NOT do:
#   - Generate the SPB synthetic dataset. That's the driver's job and
#     it happens at SPB-run time, parametrised by the scenario .properties
#     file (default: sf-0.256.properties for SF=0.256 / SPB-256).
#   - Create the scenario .properties file. Copy one of the example
#     configs that ship with the repo and customise the endpoint URL +
#     dataset paths — see the message printed at the end.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
DATA="$ROOT/crates/harness/data"
SPB_DIR="$DATA/ldbc-spb"

SPB_REPO="${SPB_REPO:-https://github.com/ldbc/ldbc_spb_bm_2.0.git}"
SPB_REF="${SPB_REF:-main}"

mkdir -p "$DATA"

if [[ ! -d "$SPB_DIR/.git" ]]; then
    echo "cloning LDBC SPB v2.0 ($SPB_REPO @ $SPB_REF)…"
    git clone --depth 1 --branch "$SPB_REF" "$SPB_REPO" "$SPB_DIR"
else
    echo "LDBC SPB already present at $SPB_DIR (skipping clone)."
fi

# Locate the ontologies dir. SPB v2.0 (master/main) puts them under
# datasets_and_queries/ontologies/{core,domain,ldbc}/. Older revisions
# used data/ontologies/ or etc/ontologies/ — probe all three, and
# accept the candidate iff it has *.ttl files anywhere under it.
# (We rely on the bootstrap script to filter ldbc/ at load time:
# that subdir is a zip-of-elsewhere-files per its own readme.txt.)
ONTOLOGIES_DIR=""
for candidate in \
    "$SPB_DIR/datasets_and_queries/ontologies" \
    "$SPB_DIR/data/ontologies" \
    "$SPB_DIR/etc/ontologies"
do
    if [[ -d "$candidate" ]] && [[ -n "$(find "$candidate" -name '*.ttl' -not -path '*/ldbc/*' -print -quit 2>/dev/null)" ]]; then
        ONTOLOGIES_DIR="$candidate"
        break
    fi
done

if [[ -z "$ONTOLOGIES_DIR" ]]; then
    echo "warning: could not find a *.ttl ontologies directory under $SPB_DIR." >&2
    echo "         Inspect the tree and set SPB_ONTOLOGIES_DIR when running bootstrap-rdfox-spb.sh." >&2
else
    echo "found SPB ontologies at: $ONTOLOGIES_DIR"
fi

# Optional driver JAR build.
DRIVER_JAR_HINT=""
if [[ "${SPB_BUILD_DRIVER:-0}" == "1" ]]; then
    if ! command -v ant >/dev/null 2>&1; then
        echo "error: SPB_BUILD_DRIVER=1 but Ant (ant) is not on PATH." >&2
        exit 2
    fi
    echo "building SPB driver JAR with Ant (this takes a few minutes)…"
    # SPB v2.0's build.xml pins javac to source/target 1.7, which JDK 9+
    # rejects ("Source option 7 is no longer supported"). Bump it to 1.8
    # (the oldest level modern JDKs still accept) so the build works on a
    # current default-jdk. Idempotent: only rewrites the 1.7 lines.
    if grep -q 'source="1.7"' "$SPB_DIR/build.xml" 2>/dev/null; then
        sed -i -e 's/source="1.7"/source="1.8"/' -e 's/target="1.7"/target="1.8"/' \
            "$SPB_DIR/build.xml"
        echo "patched build.xml: javac source/target 1.7 -> 1.8 (JDK 9+ compat)"
    fi
    # build-basic-querymix produces dist/<jar>-basic-standard.jar; that is
    # the driver the run scripts invoke as a single positional arg.
    ( cd "$SPB_DIR" && ant build-basic-querymix )
    BUILT_JAR="$(find "$SPB_DIR/dist" -maxdepth 1 -name 'semantic_publishing_benchmark-*.jar' -type f 2>/dev/null | sort | head -1)"
    if [[ -n "$BUILT_JAR" ]]; then
        # Symlink to the canonical name the run scripts default to, so the
        # common case needs no SPB_DRIVER_JAR export.
        ln -sf "$BUILT_JAR" "$SPB_DIR/spb-driver.jar"
        DRIVER_JAR_HINT="$SPB_DIR/spb-driver.jar -> $BUILT_JAR"
        echo "built: $BUILT_JAR"
        echo "symlinked: $SPB_DIR/spb-driver.jar -> $BUILT_JAR"
    fi
else
    echo "skipping driver JAR build (set SPB_BUILD_DRIVER=1 to build it now)."
fi

cat <<EOF

done.

Next steps:
  1. Build the SPB driver JAR if you haven't yet:
         (cd "$SPB_DIR" && ant build-basic-querymix)
     or rerun this script with SPB_BUILD_DRIVER=1. The built jar lands
     at $SPB_DIR/dist/semantic_publishing_benchmark-basic-standard.jar;
     symlink it to $DATA/ldbc-spb/spb-driver.jar (this script does that
     for you when it builds).

  2. Create your scenario .properties file (the SPB-256 config the
     run scripts default to looking for):
         $DATA/ldbc-spb/sf-0.256.properties
     Copy one of the example configs in $SPB_DIR and edit the
     endpoint URL + dataset paths to match your workstation.

  3. Bring up your engine:
         RDFox:        ./crates/harness/scripts/bootstrap-rdfox-spb.sh
         GraphDB Free: launch manually, create a repo named "spb"
         HornDB:       ./crates/harness/scripts/start-engine.sh (TBD)

  4. Run the benchmark:
         ./crates/harness/scripts/run-rdfox-spb-256.sh
         ./crates/harness/scripts/run-graphdb-free-spb-256.sh
         ./crates/harness/scripts/run-spb-256.sh
EOF

if [[ -n "$ONTOLOGIES_DIR" && "$ONTOLOGIES_DIR" != "$SPB_DIR/etc/ontologies" ]]; then
    echo
    echo "Note: bootstrap-rdfox-spb.sh defaults SPB_ONTOLOGIES_DIR to"
    echo "      $SPB_DIR/etc/ontologies"
    echo "      but the ontologies actually live at $ONTOLOGIES_DIR."
    echo "      Export this before running bootstrap:"
    echo "          export SPB_ONTOLOGIES_DIR='$ONTOLOGIES_DIR'"
fi

if [[ -n "$DRIVER_JAR_HINT" ]]; then
    echo
    echo "Note: driver JAR ready at the canonical path the run scripts use:"
    echo "          $DRIVER_JAR_HINT"
    echo "      No SPB_DRIVER_JAR export needed."
fi
