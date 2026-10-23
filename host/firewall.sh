#!/usr/bin/env bash
# vercelify firewall — keep the control plane private to the tailnet.
#
# Docker publishes Coolify's ports (8000 dashboard, 6001/6002 realtime,
# 8080 Traefik API), Ollama's API (11434) and the image registry (5000) on
# every interface and bypasses ufw. Rules in the DOCKER-USER chain are the
# supported way to filter that traffic.
# Public app traffic on 80/443 is untouched.
#
# Containers are also kept from reaching the rest of the tailnet (your Mac,
# other servers' agents and AI models): app code — a pull request preview,
# a compromised dependency — must not become a way into your network. Only
# what Coolify needs across servers is allowed (SSH, its API, a registry)
# plus Tailscale's DNS. Change it in /etc/vercelify/firewall.conf.
#
# Usage: firewall.sh apply | remove | status

set -euo pipefail

: "${FIREWALL_CONF:=/etc/vercelify/firewall.conf}"
# shellcheck disable=SC1090
[[ -r "$FIREWALL_CONF" ]] && . "$FIREWALL_CONF"
# block | allow — containers reaching other tailnet machines.
: "${CONTAINER_TAILNET:=block}"
# TCP ports containers may still reach on the tailnet.
: "${CONTAINER_TAILNET_ALLOW_TCP:=22 5000 8000}"
: "${DOCKER_BRIDGES:=docker0 br-+}"

: "${PRIVATE_TCP_PORTS:=8000 6001 6002 8080 11434 5000}"
# Display stream receiver (mpv) listens on UDP 5600-5610.
: "${PRIVATE_UDP_RANGE:=5600:5610}"
# The agent's wake listener for sleeping apps (only Coolify's proxy, from a
# Docker bridge, needs it).
: "${PRIVATE_INPUT_TCP:=7421}"
: "${TRUSTED_IFACES:=tailscale0 lo docker0 br-+}"

FWD=VERCELIFY-FWD
INP=VERCELIFY-IN

ipt() { "$IPT" -w "$@"; }

apply_family() {
  IPT=$1
  have_chain() { ipt -n -L "$1" >/dev/null 2>&1; }

  # Forwarded (published container ports)
  if have_chain DOCKER-USER; then
    have_chain "$FWD" || ipt -N "$FWD"
    ipt -F "$FWD"
    if [[ "$CONTAINER_TAILNET" == block ]]; then
      local b p dns=100.100.100.100
      [[ "$IPT" == ip6tables ]] && dns=fd7a:115c:a1e0::53
      ipt -A "$FWD" -o tailscale0 -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
      for b in $DOCKER_BRIDGES; do
        ipt -A "$FWD" -i "$b" -o tailscale0 -d "$dns" -j RETURN
        for p in $CONTAINER_TAILNET_ALLOW_TCP; do
          ipt -A "$FWD" -i "$b" -o tailscale0 -p tcp --dport "$p" -j RETURN
        done
        ipt -A "$FWD" -i "$b" -o tailscale0 -j DROP
      done
    fi
    for i in $TRUSTED_IFACES; do ipt -A "$FWD" -i "$i" -j RETURN; done
    for p in $PRIVATE_TCP_PORTS; do
      ipt -A "$FWD" -p tcp -m conntrack --ctorigdstport "$p" --ctdir ORIGINAL -j DROP
    done
    ipt -C DOCKER-USER -j "$FWD" 2>/dev/null || ipt -I DOCKER-USER 1 -j "$FWD"
  fi

  # Local services (display receiver, wake listener)
  have_chain "$INP" || ipt -N "$INP"
  ipt -F "$INP"
  for i in $TRUSTED_IFACES; do ipt -A "$INP" -i "$i" -j RETURN; done
  ipt -A "$INP" -p udp --dport "$PRIVATE_UDP_RANGE" -j DROP
  for p in $PRIVATE_INPUT_TCP; do ipt -A "$INP" -p tcp --dport "$p" -j DROP; done
  ipt -C INPUT -j "$INP" 2>/dev/null || ipt -I INPUT 1 -j "$INP"
}

remove_family() {
  IPT=$1
  ipt -D DOCKER-USER -j "$FWD" 2>/dev/null || true
  ipt -D INPUT -j "$INP" 2>/dev/null || true
  for c in "$FWD" "$INP"; do ipt -F "$c" 2>/dev/null || true; ipt -X "$c" 2>/dev/null || true; done
}

case "${1:-}" in
  apply)
    apply_family iptables
    command -v ip6tables >/dev/null && apply_family ip6tables || true
    echo "vercelify firewall: ports ${PRIVATE_TCP_PORTS// /,}/tcp and ${PRIVATE_UDP_RANGE/:/-}/udp limited to: $TRUSTED_IFACES"
    if [[ "$CONTAINER_TAILNET" == block ]]; then
      echo "vercelify firewall: containers reach the tailnet only on tcp ${CONTAINER_TAILNET_ALLOW_TCP// /,} (and DNS)"
    fi
    ;;
  remove)
    remove_family iptables
    command -v ip6tables >/dev/null && remove_family ip6tables || true
    echo "vercelify firewall: rules removed"
    ;;
  status)
    IPT=iptables
    ipt -n -L "$FWD" 2>/dev/null || echo "$FWD not present"
    ipt -n -L "$INP" 2>/dev/null || echo "$INP not present"
    ;;
  *) echo "usage: $0 apply|remove|status" >&2; exit 2 ;;
esac
