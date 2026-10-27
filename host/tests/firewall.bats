#!/usr/bin/env bats
# firewall.sh against a stub iptables that records the rules it is given.

setup() {
  T=$(mktemp -d)
  mkdir -p "$T/bin"
  cat >"$T/bin/iptables" <<'STUB'
#!/usr/bin/env bash
echo "${0##*/} $*" >>"$RULES"
# `-L CHAIN` / `-C …` : DOCKER-USER exists, our chains and jumps don't yet.
case " $* " in
  *" -L DOCKER-USER "*) exit 0 ;;
  *" -L "*|*" -C "*) exit 1 ;;
esac
exit 0
STUB
  cp "$T/bin/iptables" "$T/bin/ip6tables"
  chmod +x "$T/bin/"*
  export RULES="$T/rules" PATH="$T/bin:$PATH" FIREWALL_CONF="$T/firewall.conf"
  FW="$BATS_TEST_DIRNAME/../firewall.sh"
}

teardown() { rm -rf "$T"; }

@test "firewall keeps the AI API private along with the dashboard" {
  grep -q 'PRIVATE_TCP_PORTS:=8000 6001 6002 8080 11434 5000' "$FW"
}

@test "containers can't reach the mesh (or a tailnet) beyond its ACL, except Tailscale's DNS" {
  run "$FW" apply
  [ "$status" -eq 0 ]
  [[ "$output" == *"containers reach other servers only through the mesh's ACL"* ]]
  grep -q -- '^iptables -w -A VERCELIFY-FWD -i br-+ -o tailscale0 -d 100.100.100.100 -j RETURN$' "$RULES"
  ! grep -q -- '--dport 22 -j RETURN' "$RULES"
  grep -q -- '^iptables -w -A VERCELIFY-FWD -i br-+ -o vfy0 -j DROP$' "$RULES"
  grep -q -- '^iptables -w -A VERCELIFY-FWD -i docker0 -o tailscale0 -j DROP$' "$RULES"
  grep -q -- '^ip6tables -w -A VERCELIFY-FWD -i br-+ -o tailscale0 -d fd7a:115c:a1e0::53 -j RETURN$' "$RULES"
  # The isolation comes before the trusted-interface shortcut, or it would never apply.
  drop=$(grep -n -- '-i br-+ -o tailscale0 -j DROP' "$RULES" | head -1 | cut -d: -f1)
  trusted=$(grep -n -- '-A VERCELIFY-FWD -i br-+ -j RETURN' "$RULES" | head -1 | cut -d: -f1)
  [ "$drop" -lt "$trusted" ]
  grep -q -- '-I DOCKER-USER 1 -j VERCELIFY-FWD' "$RULES"
}

@test "the isolation can be turned off or widened in firewall.conf" {
  echo 'CONTAINER_TAILNET=allow' >"$FIREWALL_CONF"
  "$FW" apply >/dev/null
  ! grep -q -- '-o tailscale0' "$RULES"
  : >"$RULES"
  echo 'CONTAINER_TAILNET_ALLOW_TCP="22 5432"' >"$FIREWALL_CONF"
  "$FW" apply >/dev/null
  grep -q -- '--dport 5432 -j RETURN' "$RULES"
}

@test "the wake listener for sleeping apps is reachable from Docker bridges only" {
  "$FW" apply >/dev/null
  trusted=$(grep -n -- '-A VERCELIFY-IN -i br-+ -j RETURN' "$RULES" | head -1 | cut -d: -f1)
  drop=$(grep -n -- '-A VERCELIFY-IN -p tcp --dport 7421 -j DROP' "$RULES" | head -1 | cut -d: -f1)
  [ -n "$drop" ] && [ "$trusted" -lt "$drop" ]
}
