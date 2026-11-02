#!/usr/bin/env bash
# Research §6 steady-state method. Read-only; stdout is the raw evidence.
set -euo pipefail
samples=${SAMPLES:-12}
interval=${INTERVAL:-6}
date -u '+started %Y-%m-%dT%H:%M:%SZ'
uname -srmo
free -m
docker version --format 'Docker server {{.Server.Version}}'
for bin in /usr/local/bin/vercelify-agent /usr/local/bin/vercelify-mesh; do
  sha256sum "$bin"
done
docker ps --no-trunc --format 'container {{.Names}} image {{.Image}} id {{.ID}}'
docker image ls --digests --no-trunc
for ((i = 1; i <= samples; i++)); do
  date -u "+sample $i %Y-%m-%dT%H:%M:%SZ"
  free -m
  ps -eo comm=,rss= | awk '{rss[$1]+=$2} END {for (p in rss) printf "rss_kib %s %d\n",p,rss[p]}' | sort
  docker stats --no-stream --format 'container_memory {{.Name}} {{.MemUsage}}'
  if ((i < samples)); then sleep "$interval"; fi
done
