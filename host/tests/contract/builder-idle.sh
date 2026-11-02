#!/usr/bin/env bash
# Owns the dedicated builder; only run on a disposable Docker daemon.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib-guard.sh
. "$here/lib-guard.sh"
: "${BUILDER_CONTRACT_BIN:?point to the compiled builder_idle test executable}"
export VERCELIFY_BUILDER_CONTRACT=1
exec "$BUILDER_CONTRACT_BIN" --include-ignored --exact \
  idle_builder_retains_cache_and_restarts_for_repeated_builds --nocapture
