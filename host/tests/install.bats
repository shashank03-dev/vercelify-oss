#!/usr/bin/env bats
# install.sh, the `curl … | sudo bash` entry point.

setup() {
  T=$(mktemp -d)
  INSTALL="$BATS_TEST_DIRNAME/../../install.sh"
  # A release tarball, served by a fake curl; setup-host.sh only records it ran.
  mkdir -p "$T/src/vercelify-oss/host" "$T/bin" "$T/tmp"
  printf '#!/usr/bin/env bash\necho "setup-host ref=$VERCELIFY_INSTALL_REF sha=$VERCELIFY_INSTALL_SHA $*" >"%s/ran"\n' "$T" >"$T/src/vercelify-oss/host/setup-host.sh"
  chmod +x "$T/src/vercelify-oss/host/setup-host.sh"
  tar -czf "$T/release.tgz" -C "$T/src" vercelify-oss
  # -o FILE with -C - resumes; \$T/cut-once: the first try stops after 100
  # bytes; \$T/damaged: the server sends garbage.
  cat >"$T/bin/curl" <<CURL
#!/usr/bin/env bash
out=''; prev=''
for a in "\$@"; do [ "\$prev" = -o ] && out=\$a; prev=\$a; done
case "\$*" in
  *releases/latest*) printf '{\n  "tag_name": "v9.9.9"\n}\n' ;;
  *commits/main*) [ -e "$T/main-sha" ] && cat "$T/main-sha" || exit 22 ;;
  *codeload*)
    echo "url=\${*: -1}" >>"$T/curl.log"
    if [ -e "$T/damaged" ]; then echo garbage >"\$out"; exit 0; fi
    have=\$(wc -c <"\$out" 2>/dev/null || echo 0)
    if [ -e "$T/cut-once" ]; then rm "$T/cut-once"; head -c 100 "$T/release.tgz" >"\$out"; exit 18; fi
    echo "resumed-from=\$have" >>"$T/curl.log"
    tail -c +\$(( have + 1 )) "$T/release.tgz" >>"\$out" ;;
  *) exit 22 ;;
esac
CURL
  printf '#!/bin/sh\nexit 0\n' >"$T/bin/sleep"
  chmod +x "$T/bin/sleep"
  chmod +x "$T/bin/curl"
}

teardown() { rm -rf "$T"; }

@test "a download cut short runs none of the steps" {
  # Everything but the last line, as bash would see a connection that dropped.
  sed '$d' "$INSTALL" >"$T/cut.sh"
  run env PATH="$T/bin:$PATH" VERCELIFY_SRC_DIR="$T/opt" bash <"$T/cut.sh"
  [ ! -e "$T/opt" ]
  [ ! -e "$T/ran" ]
}

@test "the release is installed and the download is not left in /tmp" {
  [ "$EUID" -eq 0 ] || skip "install.sh needs root"
  run env PATH="$T/bin:$PATH" TMPDIR="$T/tmp" VERCELIFY_SRC_DIR="$T/opt" bash <"$INSTALL"
  [ "$status" -eq 0 ]
  [ -x "$T/opt/host/setup-host.sh" ]
  grep -q 'setup-host ref=v9.9.9' "$T/ran"
  [ -z "$(ls -A "$T/tmp")" ]
}

@test "a download that drops halfway resumes where it stopped" {
  [ "$EUID" -eq 0 ] || skip "install.sh needs root"
  touch "$T/cut-once"
  run env PATH="$T/bin:$PATH" TMPDIR="$T/tmp" VERCELIFY_SRC_DIR="$T/opt" bash <"$INSTALL"
  [ "$status" -eq 0 ]
  grep -q 'resumed-from=100' "$T/curl.log"
  grep -q 'setup-host ref=v9.9.9' "$T/ran"
}

@test "a damaged download installs nothing and says to run it again" {
  [ "$EUID" -eq 0 ] || skip "install.sh needs root"
  touch "$T/damaged"
  run env PATH="$T/bin:$PATH" TMPDIR="$T/tmp" VERCELIFY_SRC_DIR="$T/opt" bash <"$INSTALL"
  [ "$status" -ne 0 ]
  [[ "$output" == *"run the same command again"* ]]
  [ ! -e "$T/opt" ]
  [ ! -e "$T/ran" ]
}

@test "an edge install downloads the exact commit main points at, and says which" {
  [ "$EUID" -eq 0 ] || skip "install.sh needs root"
  echo 0123456789abcdef0123456789abcdef01234567 >"$T/main-sha"
  run env PATH="$T/bin:$PATH" TMPDIR="$T/tmp" VERCELIFY_SRC_DIR="$T/opt" VERCELIFY_CHANNEL=edge bash <"$INSTALL"
  [ "$status" -eq 0 ]
  grep -q 'url=.*/tar.gz/0123456789abcdef0123456789abcdef01234567' "$T/curl.log"
  grep -q 'setup-host ref=main sha=0123456789abcdef0123456789abcdef01234567' "$T/ran"
}

@test "an edge install without the commit API still installs main" {
  [ "$EUID" -eq 0 ] || skip "install.sh needs root"
  run env PATH="$T/bin:$PATH" TMPDIR="$T/tmp" VERCELIFY_SRC_DIR="$T/opt" VERCELIFY_CHANNEL=edge bash <"$INSTALL"
  [ "$status" -eq 0 ]
  grep -q 'url=.*/tar.gz/main' "$T/curl.log"
  grep -q 'setup-host ref=main sha= ' "$T/ran"
}
