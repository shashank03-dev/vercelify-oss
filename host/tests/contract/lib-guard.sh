#!/usr/bin/env bash
# Sourced by the test suites that drive this machine's Docker with their own
# agent and proxy. They replace the `vercelify-proxy` container, so on a real
# Vercelify server they would take its sites offline: refuse there.
# VERCELIFY_TESTS_ON_SERVER=1 runs them anyway.
if [[ -d "${VERCELIFY_ETC:-/etc/vercelify}" && "${VERCELIFY_TESTS_ON_SERVER:-}" != 1 ]]; then
  echo "This machine is a Vercelify server (it has ${VERCELIFY_ETC:-/etc/vercelify}): these tests replace its proxy and would take its sites offline." >&2
  echo "Run them on a machine that isn't serving apps, or set VERCELIFY_TESTS_ON_SERVER=1 to run them anyway." >&2
  exit 1
fi
