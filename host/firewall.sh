#!/usr/bin/env bash
# vercelify firewall — keep the control plane private to the mesh.
#
# Docker publishes Ollama's API (11434) on every interface and bypasses ufw.
# Rules in the DOCKER-USER chain are the supported way to filter that traffic.
# Public app traffic on 80/443 is untouched.
#
# Containers are also kept from reaching the rest of the mesh (your Mac,
# other servers' agents and AI models): app code — a pull request preview,
# a compromised dependency — must not become a way into your network. What
# they may reach on other servers (every proxy, the databases' public
# ports) comes from the mesh's ACL file below. Widen it in
# /etc/vercelify/firewall.conf.
#
# On the mesh (vfy0, design §13.8) servers may reach each other on any port,
# while devices (Macs) reach only the ports the coordinator lists for them:
# `firewall.sh mesh` renders /run/vercelify/mesh-acl.json, which the mesh
# daemon writes and runs this for on every change.
#
# Usage: firewall.sh apply | mesh | remove | status

set -euo pipefail

: "${FIREWALL_CONF:=/etc/vercelify/firewall.conf}"
# shellcheck disable=SC1090
[[ -r "$FIREWALL_CONF" ]] && . "$FIREWALL_CONF"
# block | allow — containers reaching other machines on the mesh.
: "${CONTAINER_MESH:=block}"
# TCP ports containers may still reach there, beyond the mesh's ACL.
: "${CONTAINER_MESH_ALLOW_TCP:=}"
: "${DOCKER_BRIDGES:=docker0 br-+}"

# Ollama's API and the engine's image registry (published on the mesh address).
: "${PRIVATE_TCP_PORTS:=11434 5055}"
# Display stream receiver (mpv) listens on UDP 5600-5610.
: "${PRIVATE_UDP_RANGE:=5600:5610}"
# The agent's wake listener for sleeping apps (only the proxy, from a Docker
# bridge, needs it).
: "${PRIVATE_INPUT_TCP:=7421}"
: "${TRUSTED_IFACES:=vfy0 lo docker0 br-+}"
# Private networks containers are kept off (the mesh).
: "${PRIVATE_NET_IFACES:=vfy0}"
: "${MESH_ACL:=/run/vercelify/mesh-acl.json}"
: "${MESH_IFACE:=vfy0}"

FWD=VERCELIFY-FWD
INP=VERCELIFY-IN
MESH=VERCELIFY-MESH
MESH_FWD=VERCELIFY-MESH-FWD
MESH_OUT=VERCELIFY-MESH-OUT

ipt() { "$IPT" -w "$@"; }

apply_family() {
  IPT=$1
  have_chain() { ipt -n -L "$1" >/dev/null 2>&1; }

  # Forwarded (published container ports)
  if have_chain DOCKER-USER; then
    have_chain "$FWD" || ipt -N "$FWD"
    ipt -F "$FWD"
    if [[ "$CONTAINER_MESH" == block ]]; then
      local b p o
      for o in $PRIVATE_NET_IFACES; do
        ipt -A "$FWD" -o "$o" -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
        for b in $DOCKER_BRIDGES; do
          for p in $CONTAINER_MESH_ALLOW_TCP; do
            ipt -A "$FWD" -i "$b" -o "$o" -p tcp --dport "$p" -j RETURN
          done
          ipt -A "$FWD" -i "$b" -o "$o" -j DROP
        done
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

# mesh_rules FAMILY — the rules for the ACL file, one per line, as
# iptables arguments after `-A CHAIN` (tested on their own by bats).
# FAMILY is 4 or 6. Device ports `7420/tcp`, `5600-5610/udp`.
mesh_rules() {
  local fam=$1 addr=ipv4
  [[ "$fam" == 6 ]] && addr=ipv6
  jq -r --arg a "$addr" '
    (.servers[] | select(test(if $a == "ipv4" then "^[0-9.]+$" else ":" end)) | "-s \(.) -j RETURN"),
    (.devices[] | .[$a] as $ip | select($ip != null and $ip != "") |
      ("-s \($ip) -p \(if $a == "ipv4" then "icmp" else "ipv6-icmp" end) -j RETURN"),
      (.ports[] | capture("^(?<p>[0-9]+(-[0-9]+)?)/(?<proto>tcp|udp)$") |
        "-s \($ip) -p \(.proto) --dport \(.p | sub("-"; ":")) -j RETURN"))
  ' "$MESH_ACL"
}

# mesh_out_rules FAMILY — what containers may reach on other servers
# (their proxy for the main server's forwarding, public databases; the
# coordinator decides, design §13.8), as iptables arguments.
mesh_out_rules() {
  local fam=$1 addr=ipv4
  [[ "$fam" == 6 ]] && addr=ipv6
  jq -r --arg a "$addr" '
    (.container // [])[] | .[$a] as $ip | select($ip != null and $ip != "") |
      (.ports[] | capture("^(?<p>[0-9]+(-[0-9]+)?)/(?<proto>tcp|udp)$") |
        "-d \($ip) -p \(.proto) --dport \(.p | sub("-"; ":")) -j ACCEPT")
  ' "$MESH_ACL"
}

apply_mesh_family() {
  IPT=$1
  local fam=4 rule
  [[ "$IPT" == ip6tables ]] && fam=6
  ipt -n -L "$MESH" >/dev/null 2>&1 || ipt -N "$MESH"
  ipt -F "$MESH"
  ipt -A "$MESH" -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
  while IFS= read -r rule; do
    [[ -n "$rule" ]] || continue
    # shellcheck disable=SC2086 # the rule is a list of arguments
    ipt -A "$MESH" $rule
  done < <(mesh_rules "$fam")
  ipt -A "$MESH" -j DROP
  ipt -C INPUT -i "$MESH_IFACE" -j "$MESH" 2>/dev/null || ipt -I INPUT 1 -i "$MESH_IFACE" -j "$MESH"
  # Published container ports (Traefik on 80/443…) are reached through
  # FORWARD after DNAT: the same list, on the original destination port.
  if ipt -n -L DOCKER-USER >/dev/null 2>&1; then
    ipt -n -L "$MESH_FWD" >/dev/null 2>&1 || ipt -N "$MESH_FWD"
    ipt -F "$MESH_FWD"
    ipt -A "$MESH_FWD" -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
    while IFS= read -r rule; do
      [[ -n "$rule" ]] || continue
      # shellcheck disable=SC2086
      ipt -A "$MESH_FWD" ${rule/--dport/-m conntrack --ctdir ORIGINAL --ctorigdstport}
    done < <(mesh_rules "$fam")
    ipt -A "$MESH_FWD" -j DROP
    ipt -C DOCKER-USER -i "$MESH_IFACE" -j "$MESH_FWD" 2>/dev/null || ipt -I DOCKER-USER 1 -i "$MESH_IFACE" -j "$MESH_FWD"
    # Containers out to other servers: only what the map lists; the rest
    # falls through to the mesh block in VERCELIFY-FWD.
    ipt -n -L "$MESH_OUT" >/dev/null 2>&1 || ipt -N "$MESH_OUT"
    ipt -F "$MESH_OUT"
    while IFS= read -r rule; do
      [[ -n "$rule" ]] || continue
      # shellcheck disable=SC2086
      ipt -A "$MESH_OUT" $rule
    done < <(mesh_out_rules "$fam")
    ipt -A "$MESH_OUT" -j RETURN
    ipt -C DOCKER-USER -o "$MESH_IFACE" -j "$MESH_OUT" 2>/dev/null || ipt -I DOCKER-USER 1 -o "$MESH_IFACE" -j "$MESH_OUT"
  fi
}

remove_family() {
  IPT=$1
  ipt -D DOCKER-USER -j "$FWD" 2>/dev/null || true
  ipt -D INPUT -j "$INP" 2>/dev/null || true
  ipt -D INPUT -i "$MESH_IFACE" -j "$MESH" 2>/dev/null || true
  ipt -D DOCKER-USER -i "$MESH_IFACE" -j "$MESH_FWD" 2>/dev/null || true
  ipt -D DOCKER-USER -o "$MESH_IFACE" -j "$MESH_OUT" 2>/dev/null || true
  for c in "$FWD" "$INP" "$MESH" "$MESH_FWD" "$MESH_OUT"; do ipt -F "$c" 2>/dev/null || true; ipt -X "$c" 2>/dev/null || true; done
}

case "${1:-}" in
  apply)
    apply_family iptables
    command -v ip6tables >/dev/null && apply_family ip6tables || true
    echo "vercelify firewall: ports ${PRIVATE_TCP_PORTS// /,}/tcp and ${PRIVATE_UDP_RANGE/:/-}/udp limited to: $TRUSTED_IFACES"
    if [[ "$CONTAINER_MESH" == block ]]; then
      if [[ -n "${CONTAINER_MESH_ALLOW_TCP// /}" ]]; then
        echo "vercelify firewall: containers reach other servers only through the mesh's ACL and on tcp ${CONTAINER_MESH_ALLOW_TCP// /,}"
      else
        echo "vercelify firewall: containers reach other servers only through the mesh's ACL"
      fi
    fi
    ;;
  mesh)
    [[ -r "$MESH_ACL" ]] || { echo "vercelify firewall: no $MESH_ACL (is vercelify-mesh running?)" >&2; exit 1; }
    command -v jq >/dev/null || { echo "vercelify firewall: jq is needed for the mesh rules" >&2; exit 1; }
    apply_mesh_family iptables
    command -v ip6tables >/dev/null && apply_mesh_family ip6tables || true
    echo "vercelify firewall: mesh — servers on every port, $(jq '.devices | length' "$MESH_ACL") device(s) on their ports only"
    ;;
  rules)
    # Print the mesh rules for FAMILY (4|6) without applying them.
    mesh_rules "${2:-4}"
    ;;
  out-rules)
    # Print what containers may reach on other servers, for FAMILY (4|6).
    mesh_out_rules "${2:-4}"
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
  *) echo "usage: $0 apply|mesh|remove|status" >&2; exit 2 ;;
esac
