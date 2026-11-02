#!/usr/bin/env bash
# Run the real engine/Docker preview contract on a disposable Docker host.
# Cross-compile with: cargo zigbuild --release -p vercelify-platform
#   --test preview_safety --target aarch64-unknown-linux-musl
# PREVIEW_CONTRACT_BIN=/path/to/deps/preview_safety-HASH bash this-script
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib-guard.sh
. "$here/lib-guard.sh"
: "${PREVIEW_CONTRACT_BIN:?point to the compiled preview_safety test executable}"
export VERCELIFY_PREVIEW_CONTRACT=1
exec "$PREVIEW_CONTRACT_BIN" --include-ignored --exact \
  previews_have_no_production_credentials_network_or_public_bypass --nocapture
