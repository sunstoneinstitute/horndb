#!/usr/bin/env bash
# Poll a SPARQL 1.1 query endpoint until it answers a trivial `ASK{}`,
# or give up after a timeout. The nightly workflow uses it twice — to
# gate the per-run HornDB bring-up and to probe the GraphDB Free A/B
# reference — but it is a generic liveness check any caller can use,
# including a developer bringing the engine up by hand.
#
# Usage:
#   wait-for-sparql.sh <query-endpoint> [timeout_seconds]   # default 180
#
# Exits 0 (printing a one-line "ready" note) as soon as the endpoint
# answers; exits 1 if it never does within the timeout. The caller owns
# any error reporting / log dumping.
set -euo pipefail

ENDPOINT="${1:?usage: wait-for-sparql.sh <query-endpoint> [timeout_seconds]}"
TIMEOUT="${2:-180}"

for ((i = 1; i <= TIMEOUT; i++)); do
    if curl -fsS -H 'Accept: application/sparql-results+json' \
         -G --data-urlencode 'query=ASK{}' "$ENDPOINT" >/dev/null 2>&1; then
        echo "SPARQL endpoint ready after ${i}s: $ENDPOINT"
        exit 0
    fi
    sleep 1
done
exit 1
