#!/usr/bin/env bats
# install.sh, the `curl … | sudo bash` entry point.

setup() {
  T=$(mktemp -d)
  INSTALL="$BATS_TEST_DIRNAME/../../install.sh"
  # A release tarball, served by a fake curl; setup-host.sh only records it ran.
  mkdir -p "$T/src/vercelify-oss/host" "$T/bin" "$T/tmp"
  printf '#!/usr/bin/env bash\necho "setup-host ref=$VERCELIFY_INSTALL_REF $*" >"%s/ran"\n' "$T" >"$T/src/vercelify-oss/host/setup-host.sh"
  chmod +x "$T/src/vercelify-oss/host/setup-host.sh"
  tar -czf "$T/release.tgz" -C "$T/src" vercelify-oss
  cat >"$T/bin/curl" <<CURL
#!/usr/bin/env bash
case "\$*" in
  *releases/latest*) printf '{\n  "tag_name": "v9.9.9"\n}\n' ;;
  *codeload*) cat "$T/release.tgz" ;;
  *) exit 22 ;;
esac
CURL
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
