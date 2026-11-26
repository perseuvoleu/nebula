#!/bin/sh
# Gateway.run strips every NEBULA_* variable, so the isolated dirs travel as REAL_NEBULA_STATE.
set -eu
: "${REAL_NEBULA_STATE:?}" "${REAL_NEBULA_BIN:?}"
unset NEBULA_AGENT_ID NEBULA_API_URL NEBULA_API_TOKEN
export NEBULA_RUNTIME_DIR="$REAL_NEBULA_STATE/rt" NEBULA_DATA_DIR="$REAL_NEBULA_STATE/data"
# A board host copies each decision to the RAG over ssh; a host that never resolves keeps them here.
export NEBULA_RAG_LOCAL=0 NEBULA_RAG_HOST=nebula-real-e2e.invalid
exec "$REAL_NEBULA_BIN" "$@"
