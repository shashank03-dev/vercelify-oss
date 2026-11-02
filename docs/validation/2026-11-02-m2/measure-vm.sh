#!/usr/bin/env bash
# Run inside vfybench as root after the pinned Next.js rebuild is healthy.
# Writes into the mounted workspace. Test daemon must already be stopped.
set -euo pipefail
repo=/Users/shashank/Desktop/VERCELIFY
out=/tmp/vfy-m2-memory
mkdir -p "$out"
sample=$repo/docs/validation/2026-11-02-m1/sample-memory.sh
[[ $(docker inspect -f '{{.State.Running}}' vfy-m1-contract-docker) == false ]]
[[ $(docker inspect -f '{{.State.Running}}' buildx_buildkit_vercelify0) == true ]]
if [[ ${SETTLED:-0} != 1 ]]; then sleep 60; sleep 60; fi
bash "$sample" > "$out/builder-running.txt"
# Restarting grants a fresh idle period. With 1 minute configured the 30s
# task must stop the builder within 120s; record its actual observation.
sed -i 's/^builder_idle_minutes = 0$/builder_idle_minutes = 1/' /etc/vercelify/agent.toml
systemctl restart vercelify-agent
start=$(date +%s)
while [[ $(docker inspect -f '{{.State.Running}}' buildx_buildkit_vercelify0) == true ]]; do
  if (( $(date +%s) - start > 120 )); then echo 'idle stop missed timeout + 60s' >&2; exit 1; fi
  sleep 1
done
elapsed=$(( $(date +%s) - start ))
echo "automatic stop observed ${elapsed}s after agent restart; configured timeout 60s" > "$out/idle-stop.txt"
sleep 60
sleep 60
bash "$sample" > "$out/builder-stopped.txt"
# Restore the documented default for subsequent milestones.
sed -i '/^builder_idle_minutes =/d' /etc/vercelify/agent.toml
systemctl restart vercelify-agent
