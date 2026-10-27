# Common bats helpers: a fake sysfs tree and a stub docker that records calls.

setup_sandbox() {
  SANDBOX=$(mktemp -d)
  export SANDBOX
  export POWER_SUPPLY_DIR="$SANDBOX/sys/class/power_supply"
  export SYS_CLASS_DIR="$SANDBOX/sys/class"
  export STATE_DIR="$SANDBOX/run"
  export PERSIST_DIR="$SANDBOX/var"
  export POWER_GUARD_CONFIG="$SANDBOX/power-guard.conf"
  export POWER_GUARD_LOCAL_CONFIG="$SANDBOX/power-guard.local.conf"
  export CALLS="$SANDBOX/calls.log"
  export SHUTDOWN_CMD="$SANDBOX/bin/fake-poweroff"
  mkdir -p "$POWER_SUPPLY_DIR" "$SANDBOX/bin" "$STATE_DIR" "$PERSIST_DIR"
  : >"$CALLS"

  # Stub docker: `ps` prints $SANDBOX/containers ("id name" lines).
  cat >"$SANDBOX/bin/docker" <<'STUB'
#!/usr/bin/env bash
echo "docker $*" >>"$CALLS"
case "$1" in
  ps) cat "$SANDBOX/containers" 2>/dev/null ;;
  inspect) echo "${STUB_NANOCPUS:-0}" ;;
  info) exit 0 ;;
esac
STUB
  cat >"$SANDBOX/bin/fake-poweroff" <<'STUB'
#!/usr/bin/env bash
echo "POWEROFF" >>"$CALLS"
STUB
  chmod +x "$SANDBOX/bin/"*
  export DOCKER="$SANDBOX/bin/docker"
  printf 'aaa app-web node:22\nbbb vercelify-proxy traefik:v3.1\nccc postgres-xyz postgres:16-alpine\nddd vfy-registry registry:2\n' >"$SANDBOX/containers"
}

teardown_sandbox() { rm -rf "$SANDBOX"; }

set_attr() { mkdir -p "$(dirname "$1")"; printf '%s\n' "$2" >"$1"; }

laptop() { # laptop AC_ONLINE CAPACITY STATUS
  set_attr "$POWER_SUPPLY_DIR/AC/type" Mains
  set_attr "$POWER_SUPPLY_DIR/AC/online" "$1"
  set_attr "$POWER_SUPPLY_DIR/BAT0/type" Battery
  set_attr "$POWER_SUPPLY_DIR/BAT0/present" 1
  set_attr "$POWER_SUPPLY_DIR/BAT0/capacity" "$2"
  set_attr "$POWER_SUPPLY_DIR/BAT0/status" "$3"
}

cpu_temp() { set_attr "$SYS_CLASS_DIR/thermal/thermal_zone0/temp" "$(( $1 * 1000 ))"; set_attr "$SYS_CLASS_DIR/thermal/thermal_zone0/type" x86_pkg_temp; }

state() { sed -E 's/.*"state":"([^"]+)".*/\1/' "$STATE_DIR/power-guard.state"; }
