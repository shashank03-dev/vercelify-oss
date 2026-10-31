#!/usr/bin/env bats
bats_require_minimum_version 1.5.0
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
  # iptables-restore: one recorded line per input line, after "restore:".
  printf '#!/usr/bin/env bash\nwhile IFS= read -r l; do echo "${0##*/}: $l" >>"$RULES"; done\necho "${0##*/}: END" >>"$RULES"\n' >"$T/bin/iptables-restore"
  cp "$T/bin/iptables-restore" "$T/bin/ip6tables-restore"
  cp "$T/bin/iptables" "$T/bin/ip6tables"
  chmod +x "$T/bin/"*
  export RULES="$T/rules" PATH="$T/bin:$PATH" FIREWALL_CONF="$T/firewall.conf"
  FW="$BATS_TEST_DIRNAME/../firewall.sh"
}

teardown() { rm -rf "$T"; }

@test "firewall keeps the AI API and the image registry private" {
  grep -q 'PRIVATE_TCP_PORTS:=11434 5055' "$FW"
}

@test "containers can't reach the mesh beyond its ACL" {
  run "$FW" apply
  [ "$status" -eq 0 ]
  [[ "$output" == *"containers reach other servers only through the mesh's ACL"* ]]
  run ! grep -q -- '--dport 22 -j RETURN' "$RULES"
  grep -q -- '^iptables -w -A VERCELIFY-FWD -i br-+ -o vfy0 -j DROP$' "$RULES"
  grep -q -- '^iptables -w -A VERCELIFY-FWD -i docker0 -o vfy0 -j DROP$' "$RULES"
  grep -q -- '^ip6tables -w -A VERCELIFY-FWD -i br-+ -o vfy0 -j DROP$' "$RULES"
  # The isolation comes before the trusted-interface shortcut, or it would never apply.
  drop=$(grep -n -- '-i br-+ -o vfy0 -j DROP' "$RULES" | head -1 | cut -d: -f1)
  trusted=$(grep -n -- '-A VERCELIFY-FWD -i br-+ -j RETURN' "$RULES" | head -1 | cut -d: -f1)
  [ "$drop" -lt "$trusted" ]
  grep -q -- '-I DOCKER-USER 1 -j VERCELIFY-FWD' "$RULES"
}

@test "the isolation can be turned off or widened in firewall.conf" {
  echo 'CONTAINER_MESH=allow' >"$FIREWALL_CONF"
  "$FW" apply >/dev/null
  run ! grep -q -- '-o vfy0 -j DROP' "$RULES"
  : >"$RULES"
  echo 'CONTAINER_MESH_ALLOW_TCP="22 5432"' >"$FIREWALL_CONF"
  "$FW" apply >/dev/null
  grep -q -- '--dport 5432 -j RETURN' "$RULES"
}

@test "the wake listener for sleeping apps is reachable from Docker bridges only" {
  "$FW" apply >/dev/null
  trusted=$(grep -n -- '-A VERCELIFY-IN -i br-+ -j RETURN' "$RULES" | head -1 | cut -d: -f1)
  drop=$(grep -n -- '-A VERCELIFY-IN -p tcp --dport 7421 -j DROP' "$RULES" | head -1 | cut -d: -f1)
  [ -n "$drop" ] && [ "$trusted" -lt "$drop" ]
}

acl() {
  cat >"$T/acl.json" <<'JSON'
{"interface":"vfy0","version":3,"servers":["10.77.0.1"],
 "devices":[{"name":"mac","ipv4":"10.77.0.4","ports":["7420/tcp","80/tcp","443/tcp"]}]}
JSON
}

@test "a firewall restart rebuilds the mesh's chains too, so devices are never left unfiltered" {
  command -v jq >/dev/null || skip "needs jq"
  acl
  MESH_ACL="$T/acl.json" FIREWALL_LOCK="$T/fw.lock" run "$FW" apply
  [ "$status" -eq 0 ]
  [[ "$output" == *"mesh rules re-applied"* ]]
  grep -q -- '^iptables-restore: -A VERCELIFY-MESH -s 10.77.0.4 -p tcp --dport 7420 -j RETURN$' "$RULES"
  grep -q -- '^iptables-restore: -A VERCELIFY-MESH -j DROP$' "$RULES"
}

@test "a mesh update replaces its chains in one commit: never empty, never half-built" {
  command -v jq >/dev/null || skip "needs jq"
  acl
  export MESH_ACL="$T/acl.json" FIREWALL_LOCK="$T/fw.lock"
  rm -f "$T/bin/ip6tables" "$T/bin/ip6tables-restore"
  "$FW" mesh >/dev/null
  "$FW" mesh >/dev/null
  # No rule-by-rule flush and refill of the mesh chains…
  ! grep -qE '^iptables -w -(F|A) VERCELIFY-MESH' "$RULES"
  # …each update is one restore that declares the chain, fills it and ends with its DROP.
  seq=$(grep '^iptables-restore: ' "$RULES" | sed -E 's/^iptables-restore: //' | grep -E '^(:VERCELIFY-MESH |-A VERCELIFY-MESH |COMMIT|END)' | sed -E 's/^:.*/NEW/; s/.*-j DROP$/DROP/; s/^-A .*/R/' | tr '\n' ' ')
  [[ "$seq" =~ ^NEW\ (R\ )+DROP\ COMMIT\ END\ NEW\ (R\ )+DROP\ COMMIT\ END\ $ ]] || { echo "$seq"; false; }
}
