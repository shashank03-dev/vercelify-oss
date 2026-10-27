#!/usr/bin/env bash
# Building for the other CPU type. (The engine runs its own registry on the
# main server for the mesh, design §8.2: there is nothing to install or trust.)

# Build arm64 images on x86 (and the reverse) through QEMU emulation.
registry_multiarch() {
  run_cmd docker run --privileged --rm tonistiigi/binfmt --install arm64,amd64 >/dev/null ||
    die "could not register the emulators"
  ok "this machine can now build linux/arm64 and linux/amd64 images (slower than native)"
}

registry_cli() {
  case "${1:-}" in
    multiarch) registry_multiarch ;;
    install|trust) die "Vercelify's engine runs its own registry on the main server: workers pull from it over the mesh, with nothing to set up" ;;
    *) die "usage: vercelify registry multiarch" ;;
  esac
}
