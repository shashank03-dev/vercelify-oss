#!/usr/bin/env bash
# Shared helpers for Vercelify host scripts. Source, don't execute.
# All file locations are overridable so the test-suite can run unprivileged.

: "${VERCELIFY_ETC:=/etc/vercelify}"
: "${VERCELIFY_LIB:=/usr/local/lib/vercelify}"
: "${VERCELIFY_LOG:=/var/log/vercelify-setup.log}"
: "${OS_RELEASE_FILE:=/etc/os-release}"
: "${FSTAB_FILE:=/etc/fstab}"
: "${SYSCTL_DIR:=/etc/sysctl.d}"
: "${SYSTEMD_DIR:=/etc/systemd/system}"
: "${DOCKER_DAEMON_JSON:=/etc/docker/daemon.json}"
: "${STORAGE_MOUNT:=/mnt/storage}"
: "${VERCELIFY_DRY_RUN:=0}"

if [[ -t 2 && -z "${NO_COLOR:-}" ]]; then
  C_RESET=$'\e[0m'; C_BOLD=$'\e[1m'; C_DIM=$'\e[2m'
  C_RED=$'\e[31m'; C_GREEN=$'\e[32m'; C_YELLOW=$'\e[33m'; C_BLUE=$'\e[34m'; C_CYAN=$'\e[36m'
else
  C_RESET=''; C_BOLD=''; C_DIM=''; C_RED=''; C_GREEN=''; C_YELLOW=''; C_BLUE=''; C_CYAN=''
fi

_log_file() { [[ -w "$(dirname "$VERCELIFY_LOG")" ]] && printf '%s %s\n' "$(date -Is)" "$*" >>"$VERCELIFY_LOG" 2>/dev/null || true; }
step()  { printf '\n%s==>%s %s%s%s\n' "$C_BLUE" "$C_RESET" "$C_BOLD" "$*" "$C_RESET" >&2; _log_file "STEP $*"; }
info()  { printf '  %s•%s %s\n' "$C_CYAN" "$C_RESET" "$*" >&2; _log_file "INFO $*"; }
ok()    { printf '  %s✓%s %s\n' "$C_GREEN" "$C_RESET" "$*" >&2; _log_file "OK   $*"; }
warn()  { printf '  %s!%s %s\n' "$C_YELLOW" "$C_RESET" "$*" >&2; _log_file "WARN $*"; }
err()   { printf '  %s✗ %s%s\n' "$C_RED" "$*" "$C_RESET" >&2; _log_file "ERR  $*"; }
die()   { err "$*"; exit 1; }

# run_cmd CMD... — executes, or prints in VERCELIFY_DRY_RUN mode.
run_cmd() {
  if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then
    printf '  %s[dry-run]%s %s\n' "$C_DIM" "$C_RESET" "$*" >&2
    return 0
  fi
  _log_file "RUN  $*"
  "$@"
}

have() { command -v "$1" >/dev/null 2>&1; }

# Interactive input that works under `curl ... | sudo bash`.
has_tty() { [[ "${VERCELIFY_NONINTERACTIVE:-0}" != 1 ]] && { : </dev/tty; } 2>/dev/null; }

# ask VAR "Prompt" [default]
ask() {
  local __var=$1 prompt=$2 def=${3:-} reply=''
  if has_tty; then
    if [[ -n "$def" ]]; then
      printf '  %s?%s %s %s[%s]%s ' "$C_YELLOW" "$C_RESET" "$prompt" "$C_DIM" "$def" "$C_RESET" >/dev/tty
    else
      printf '  %s?%s %s ' "$C_YELLOW" "$C_RESET" "$prompt" >/dev/tty
    fi
    IFS= read -r reply </dev/tty || true
  fi
  printf -v "$__var" '%s' "${reply:-$def}"
}

# ask_secret VAR "Prompt"
ask_secret() {
  local __var=$1 prompt=$2 reply=''
  if has_tty; then
    printf '  %s?%s %s ' "$C_YELLOW" "$C_RESET" "$prompt" >/dev/tty
    IFS= read -rs reply </dev/tty || true
    printf '\n' >/dev/tty
  fi
  printf -v "$__var" '%s' "$reply"
}

# confirm "Question" [default y|n] — returns 0 for yes.
confirm() {
  local q=$1 def=${2:-n} reply
  if [[ "${ASSUME_YES:-0}" == 1 ]]; then return 0; fi
  if ! has_tty; then [[ "$def" == y ]]; return; fi
  ask reply "$q $( [[ $def == y ]] && echo '[Y/n]' || echo '[y/N]')" ""
  reply=${reply:-$def}
  [[ "${reply,,}" == y || "${reply,,}" == yes ]]
}

# --- OS detection ----------------------------------------------------------

# Sets OS_ID, OS_VERSION_ID, OS_PRETTY, OS_LIKE.
detect_os() {
  [[ -r "$OS_RELEASE_FILE" ]] || die "cannot read $OS_RELEASE_FILE"
  local vars
  # shellcheck disable=SC1090
  vars=$(. "$OS_RELEASE_FILE"; printf 'OS_ID=%q OS_VERSION_ID=%q OS_PRETTY=%q OS_LIKE=%q' \
    "${ID:-}" "${VERSION_ID:-}" "${PRETTY_NAME:-${ID:-}}" "${ID_LIKE:-}")
  eval "$vars"
}

# Returns 0 for OSes we test against; 1 (warning) for other Debian-likes; 2 unsupported.
os_support_level() {
  case "$OS_ID:$OS_VERSION_ID" in
    debian:12|debian:13|ubuntu:22.04|ubuntu:24.04|raspbian:12) return 0 ;;
  esac
  case "$OS_ID $OS_LIKE" in
    *debian*|*ubuntu*) return 1 ;;
  esac
  return 2
}

# Normalised CPU architecture: amd64 | arm64 | other
detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo amd64 ;;
    aarch64|arm64) echo arm64 ;;
    *) uname -m ;;
  esac
}

rust_target() {
  case "$1" in
    amd64) echo x86_64-unknown-linux-musl ;;
    arm64) echo aarch64-unknown-linux-musl ;;
    *) return 1 ;;
  esac
}

# --- small file helpers ----------------------------------------------------

# write_file PATH MODE < content — writes only when content changed.
# Returns 0 if changed, 1 if unchanged.
write_file() {
  local path=$1 mode=${2:-0644} tmp
  tmp=$(mktemp)
  cat >"$tmp"
  if [[ -f "$path" ]] && cmp -s "$tmp" "$path"; then
    rm -f "$tmp"; return 1
  fi
  if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then
    printf '  %s[dry-run]%s write %s\n' "$C_DIM" "$C_RESET" "$path" >&2
    rm -f "$tmp"; return 0
  fi
  mkdir -p "$(dirname "$path")"
  install -m "$mode" "$tmp" "$path"
  rm -f "$tmp"
  return 0
}

# Strong random hex string.
random_hex() { od -An -N"${1:-32}" -tx1 /dev/urandom | tr -d ' \n'; }

# retry N DELAY CMD... — exponential backoff.
retry() {
  local n=$1 delay=$2 i=1; shift 2
  until "$@"; do
    (( i >= n )) && return 1
    sleep "$delay"; delay=$(( delay * 2 )); i=$(( i + 1 ))
  done
}

# Download URL to FILE with retries.
fetch() { retry 4 2 curl -fsSL --connect-timeout 15 --retry 2 -o "$2" "$1"; }
