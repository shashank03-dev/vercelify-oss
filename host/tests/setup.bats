#!/usr/bin/env bats
# Unit tests for the installer's decision logic (no root, no real disks).

setup() {
  T=$(mktemp -d)
  export VERCELIFY_SOURCE_ONLY=1 VERCELIFY_DRY_RUN=0 NO_COLOR=1
  export VERCELIFY_LOG="$T/setup.log" FSTAB_FILE="$T/fstab" DOCKER_DAEMON_JSON="$T/daemon.json"
  export OS_RELEASE_FILE="$T/os-release" STORAGE_MOUNT=/mnt/storage
  printf '# /etc/fstab\nUUID=1111 / ext4 errors=remount-ro 0 1\n' >"$FSTAB_FILE"
  # shellcheck source=../setup-host.sh
  . "$BATS_TEST_DIRNAME/../setup-host.sh"
  FIX="$BATS_TEST_DIRNAME/fixtures"
  S=$FS_SEP
}

teardown() { rm -rf "$T"; }

# Run an installer function exactly as the installer does: strict mode on.
strict() {
  STUB_BIN="$T/bin" bash -c 'VERCELIFY_SOURCE_ONLY=1 . "$1"; shift; set -Eeuo pipefail; PATH="$STUB_BIN:$PATH"; "$@"' _ "$BATS_TEST_DIRNAME/../setup-host.sh" "$@"
}

os() { printf 'ID=%s\nVERSION_ID="%s"\nPRETTY_NAME="%s"\nID_LIKE="%s"\n' "$1" "$2" "$1 $2" "${3:-}" >"$OS_RELEASE_FILE"; detect_os; }

@test "supported operating systems" {
  os debian 12;   run os_support_level; [ "$status" -eq 0 ]
  os ubuntu 24.04; run os_support_level; [ "$status" -eq 0 ]
  os raspbian 12; run os_support_level; [ "$status" -eq 0 ]
  os linuxmint 21 "ubuntu debian"; run os_support_level; [ "$status" -eq 1 ]
  os fedora 40;   run os_support_level; [ "$status" -eq 2 ]
}

@test "laptop: picks the HDD, never the OS disk, USB stick or zram" {
  export LSBLK_JSON="$FIX/lsblk-laptop.json"
  run storage_candidates
  [ "$status" -eq 0 ]
  [ "${#lines[@]}" -eq 1 ]
  [[ "${lines[0]}" == /dev/sdb${S}1000204886016${S}hdd${S}ST1000LM035-1RK172${S}sata* ]]
}

@test "laptop: target is the existing ext4 partition" {
  export LSBLK_JSON="$FIX/lsblk-laptop.json"
  IFS=$FS_SEP read -r path fs uuid mounts kind < <(storage_target /dev/sdb)
  [ "$path" = /dev/sdb1 ]
  [ "$fs" = ext4 ]
  [ "$uuid" = aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee ]
  [ -z "$mounts" ]
  [ "$kind" = part ]
}

@test "blank HDD next to NVMe: whole disk, needs formatting" {
  export LSBLK_JSON="$FIX/lsblk-blank-hdd.json"
  run storage_candidates
  [ "${#lines[@]}" -eq 1 ]
  [[ "${lines[0]}" == /dev/sda$S* ]]
  IFS=$FS_SEP read -r path fs uuid mounts kind < <(storage_target /dev/sda)
  [ "$path" = /dev/sda ] && [ -z "$fs" ] && [ "$kind" = disk ]
}

@test "old lsblk JSON (strings, single mountpoint) on a Raspberry Pi" {
  export LSBLK_JSON="$FIX/lsblk-old-format.json"
  run storage_candidates
  [ "${#lines[@]}" -eq 1 ]
  [[ "${lines[0]}" == /dev/sda${S}500107862016${S}hdd${S}"USB HDD"${S}usb${S}/media/pi/Backup ]]
  IFS=$FS_SEP read -r path fs uuid mounts kind < <(storage_target /dev/sda)
  [ "$path" = /dev/sda1 ] && [ "$fs" = ntfs ] && [ "$mounts" = /media/pi/Backup ]
}

@test "partition paths" {
  [ "$(partition_path /dev/sdb 1)" = /dev/sdb1 ]
  [ "$(partition_path /dev/nvme1n1 1)" = /dev/nvme1n1p1 ]
  [ "$(partition_path /dev/mmcblk0 1)" = /dev/mmcblk0p1 ]
}

@test "fstab: storage line uses UUID + nofail and is idempotent" {
  line=$(fstab_storage_line aaaa-bbbb ext4)
  [ "$line" = "UUID=aaaa-bbbb /mnt/storage ext4 defaults,nofail,noatime,x-systemd.device-timeout=10s 0 2" ]
  fstab_set /mnt/storage "$line"
  run fstab_set /mnt/storage "$line"
  [ "$status" -eq 1 ]   # unchanged
  [ "$(grep -c '/mnt/storage' "$FSTAB_FILE")" -eq 2 ]   # marker + entry
  grep -q '^UUID=1111 / ext4' "$FSTAB_FILE"
  [ -f "$FSTAB_FILE.vercelify.bak" ]
}

@test "fstab: replaces a previous entry for the same mount point" {
  echo "/dev/sdb1 /mnt/storage ext4 defaults 0 0" >>"$FSTAB_FILE"
  fstab_set /mnt/storage "$(fstab_storage_line new-uuid ext4)"
  [ "$(grep -c '^/dev/sdb1' "$FSTAB_FILE")" -eq 0 ]
  [ "$(grep -c 'UUID=new-uuid' "$FSTAB_FILE")" -eq 1 ]
  fstab_set /mnt/storage "$(fstab_storage_line newer-uuid ext4)"
  [ "$(grep -c 'UUID=new' "$FSTAB_FILE")" -eq 1 ]
  [ "$(grep -c '^# vercelify /mnt/storage' "$FSTAB_FILE")" -eq 1 ]
}

@test "daemon.json: created with log limits, address pools and a shutdown timeout" {
  DOCKER_POOL=10.0.0.0/8 strict docker_daemon_config
  [ "$(jq -r '."log-opts"."max-size"' "$DOCKER_DAEMON_JSON")" = 10m ]
  [ "$(jq -r '."default-address-pools"[0].base' "$DOCKER_DAEMON_JSON")" = 10.0.0.0/8 ]
  [ "$(jq -r '."shutdown-timeout"' "$DOCKER_DAEMON_JSON")" = 30 ]
  run docker_daemon_config
  [ "$status" -eq 1 ]   # second run: unchanged
}

@test "daemon.json: existing settings win and are preserved" {
  echo '{"registry-mirrors":["https://mirror.example"],"log-opts":{"max-size":"50m"},"default-address-pools":[{"base":"172.80.0.0/16","size":24}]}' >"$DOCKER_DAEMON_JSON"
  DOCKER_POOL=10.0.0.0/8 docker_daemon_config
  [ "$(jq -r '."registry-mirrors"[0]' "$DOCKER_DAEMON_JSON")" = https://mirror.example ]
  [ "$(jq -r '."log-opts"."max-size"' "$DOCKER_DAEMON_JSON")" = 50m ]
  [ "$(jq -r '."log-opts"."max-file"' "$DOCKER_DAEMON_JSON")" = 3 ]
  [ "$(jq -r '."default-address-pools"[0].base' "$DOCKER_DAEMON_JSON")" = 172.80.0.0/16 ]
}

@test "daemon.json: invalid JSON is refused, not overwritten" {
  echo '{ broken' >"$DOCKER_DAEMON_JSON"
  run docker_daemon_config
  [ "$status" -ne 0 ]
  [ "$(cat "$DOCKER_DAEMON_JSON")" = '{ broken' ]
}

@test "argument parsing normalises the domain" {
  parse_args --domain 'HTTPS://*.Example.com/' --hostname laptop -y
  [ "$DOMAIN" = example.com ]
  [ "$MACHINE_NAME" = laptop ] && [ "$RENAME" = 1 ]
  [ "$ASSUME_YES" = 1 ]
  valid_domain example.com
  valid_domain apps.my-site.co.uk
  run valid_domain 'bad domain'; [ "$status" -ne 0 ]
  run valid_domain localhost; [ "$status" -ne 0 ]
}

@test "a worker joins with the invite the main server printed" {
  parse_args --worker --join vfyj1.abc --hostname pi5 --ai
  [ "$ROLE" = worker ] && [ "$JOIN_TOKEN" = vfyj1.abc ] && [ "$MACHINE_NAME" = pi5 ] && [ "$WITH_AI" = 1 ]
}

@test "unknown options are refused" {
  run parse_args --bogus-option -y
  [ "$status" -ne 0 ]
  [[ "$output" == *"unknown option: --bogus-option"* ]]
}

@test "app wildcard per domain mode" {
  DOMAIN=example.com CF_API_TOKEN=x; [ "$(app_wildcard)" = http://example.com ]
  DOMAIN=example.com CF_API_TOKEN=''; [ "$(app_wildcard)" = https://example.com ]
  # private: the mesh address, through sslip.io
  mesh_ipv4() { echo 10.77.0.1; }
  DOMAIN='' CF_API_TOKEN=''; [ "$(app_wildcard)" = http://10.77.0.1.sslip.io ]
  mesh_ipv4() { return 1; }
  [ "$(app_wildcard)" = "http://$MESH_MAIN_IP.sslip.io" ]
}

@test "--help works without root" {
  run env VERCELIFY_SOURCE_ONLY=0 bash "$BATS_TEST_DIRNAME/../setup-host.sh" --help
  [ "$status" -eq 0 ]
  [[ "$output" == *"--cloudflare-token"* ]]
}

@test "unknown options are rejected" {
  run env VERCELIFY_SOURCE_ONLY=0 bash "$BATS_TEST_DIRNAME/../setup-host.sh" --bogus
  [ "$status" -ne 0 ]
  [[ "$output" == *"unknown option: --bogus"* ]]
}

stub_destructive_tools() {
  mkdir -p "$T/bin"
  for t in wipefs parted mkfs.ext4 partx; do
    printf '#!/bin/sh\necho "%s $*" >> "%s/destructive.log"\n' "$t" "$T" >"$T/bin/$t"
    chmod +x "$T/bin/$t"
  done
  PATH="$T/bin:$PATH"
}

@test "SAFETY: -y never authorises formatting a blank disk" {
  stub_destructive_tools
  export LSBLK_JSON="$FIX/lsblk-blank-hdd.json" ASSUME_YES=1 VERCELIFY_NONINTERACTIVE=1 STORAGE_MOUNT="$T/mnt"
  run strict setup_storage /dev/sda 0
  [ "$status" -ne 0 ]
  [[ "$output" == *"refusing to format /dev/sda without explicit consent"* ]]
  [ ! -e "$T/destructive.log" ]
}

@test "SAFETY: NTFS disks are not reformatted without --format-storage" {
  stub_destructive_tools
  export LSBLK_JSON="$FIX/lsblk-laptop.json" ASSUME_YES=1 VERCELIFY_NONINTERACTIVE=1 STORAGE_MOUNT="$T/mnt"
  # make sdb1 look like NTFS
  jq '(.blockdevices[] | select(.name=="sdb") | .children[0].fstype) = "ntfs"' "$FIX/lsblk-laptop.json" >"$T/ntfs.json"
  export LSBLK_JSON="$T/ntfs.json"
  run strict setup_storage /dev/sdb 0
  [ "$status" -ne 0 ]
  [[ "$output" == *"uses ntfs; re-run with --format-storage"* ]]
  [ ! -e "$T/destructive.log" ]
}

@test "existing ext4 partition is reused, never formatted" {
  stub_destructive_tools
  export LSBLK_JSON="$FIX/lsblk-laptop.json" ASSUME_YES=1 VERCELIFY_NONINTERACTIVE=1 STORAGE_MOUNT="$T/mnt" VERCELIFY_DRY_RUN=1
  run strict setup_storage /dev/sdb 0
  [ "$status" -eq 0 ]
  [[ "$output" == *"/dev/sdb → /dev/sdb1 (ext4)"* ]]
  [[ "$output" == *"UUID=aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee $T/mnt ext4 defaults,nofail"* ]]
  [ ! -e "$T/destructive.log" ]
}

@test "--format-storage on a blank disk partitions and formats it" {
  stub_destructive_tools
  export LSBLK_JSON="$FIX/lsblk-blank-hdd.json" VERCELIFY_NONINTERACTIVE=1 STORAGE_MOUNT="$T/mnt"
  # format_device only; the stubs record what would run
  run strict format_device /dev/sda disk /dev/sda
  grep -q "^wipefs -a /dev/sda" "$T/destructive.log"
  grep -q "^parted -s /dev/sda mklabel gpt mkpart vstorage ext4 1MiB 100%" "$T/destructive.log"
  grep -q "^mkfs.ext4 -F -q -L vstorage -m 1 /dev/sda" "$T/destructive.log"
}

@test "updates keep the domain chosen earlier (no --domain on re-run)" {
  export VERCELIFY_ETC="$T/etc" SYSTEMD_DIR="$T/systemd"
  mkdir -p "$VERCELIFY_ETC" "$SYSTEMD_DIR"
  mesh_ipv4() { echo 10.77.0.1; }

  printf 'role=main\nversion=0.1.0\ndomain=example.com\ndomain_mode=cloudflare\n' >"$VERCELIFY_ETC/install.env"
  DOMAIN='' CF_API_TOKEN='' DOMAIN_MODE=''
  load_previous_install
  [ "$DOMAIN" = example.com ]
  [ "$(domain_mode)" = cloudflare ]
  [ "$(app_wildcard)" = http://example.com ]

  printf 'role=main\ndomain=example.org\ndomain_mode=direct\n' >"$VERCELIFY_ETC/install.env"
  DOMAIN='' DOMAIN_MODE=''
  load_previous_install
  [ "$(app_wildcard)" = https://example.org ]

  # 0.1 installs wrote no domain_mode: a tunnel unit means Cloudflare
  printf 'role=main\ndomain=example.net\n' >"$VERCELIFY_ETC/install.env"
  touch "$SYSTEMD_DIR/cloudflared.service"
  DOMAIN='' DOMAIN_MODE=''
  load_previous_install
  [ "$(domain_mode)" = cloudflare ]

  # an explicit --domain wins
  DOMAIN=new.dev DOMAIN_MODE=''
  load_previous_install
  [ "$DOMAIN" = new.dev ] && [ "$(domain_mode)" = direct ]

  # no domain at all stays private
  printf 'role=main\ndomain=\ndomain_mode=\n' >"$VERCELIFY_ETC/install.env"
  DOMAIN='' DOMAIN_MODE=''
  load_previous_install
  [ -z "$DOMAIN" ] && [ "$(app_wildcard)" = http://10.77.0.1.sslip.io ]
}

@test "--ai with and without a model" {
  parse_args --ai -y
  [ "$WITH_AI" = 1 ] && [ -z "$AI_MODEL" ]
  WITH_AI=0
  parse_args --ai qwen2.5:3b --hostname box
  [ "$WITH_AI" = 1 ] && [ "$AI_MODEL" = qwen2.5:3b ] && [ "$MACHINE_NAME" = box ]
  run default_ai_model
  [[ "$output" == llama3.2:3b || "$output" == gemma3:1b ]]
}

@test "network: the main server coordinates, a worker joins with its invite" {
  export VERCELIFY_ETC="$T/etc" MESH_DIR="$T/mesh"
  mkdir -p "$VERCELIFY_ETC"
  mesh_init() { echo "mesh_init"; }
  mesh_join() { echo "mesh_join $*"; }
  ROLE=main JOIN_TOKEN=''
  run setup_network
  [[ "$output" == *"mesh_init"* ]]
  ROLE=worker JOIN_TOKEN=vfyj1.abc
  run setup_network
  [[ "$output" == *"mesh_join vfyj1.abc --platform"* ]]
  # a worker without an invite says what to do
  JOIN_TOKEN=''
  run setup_network
  [[ "$output" == *"sudo vercelify mesh invite"* ]]
  grep -q '^engine = "platform"' "$VERCELIFY_ETC/agent.toml"
  # already on the network: nothing joins twice
  mkdir -p "$MESH_DIR" && echo '{"ipv4":"10.77.0.4"}' >"$MESH_DIR/state.json"
  systemctl() { return 0; }
  ROLE=worker JOIN_TOKEN=vfyj1.abc
  run setup_network
  [[ "$output" == *"on the network as 10.77.0.4"* && "$output" != *mesh_join* ]]
}
