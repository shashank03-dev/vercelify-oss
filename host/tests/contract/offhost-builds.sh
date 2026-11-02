#!/usr/bin/env bash
# Builder is the caller's selected Docker runtime. Target is disposable and may
# be reached through OFFHOST_TARGET_EXEC (a JSON argv prefix, never a shell string).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib-guard.sh
. "$here/lib-guard.sh"
exec python3 "$here/offhost-builds.py"
