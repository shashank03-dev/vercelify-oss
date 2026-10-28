#!/usr/bin/env bash
# Storage tier: detect the second disk, (optionally) format it, mount it at
# /mnt/storage with `nofail`, and guard the empty mount point.

: "${LSBLK_JSON:=}"
STORAGE_LABEL=vstorage
FSTAB_MARK="# vercelify"

lsblk_json() {
  if [[ -n "$LSBLK_JSON" ]]; then cat "$LSBLK_JSON"; return; fi
  lsblk -J -b -o NAME,PATH,TYPE,SIZE,FSTYPE,UUID,MOUNTPOINTS,RM,ROTA,MODEL,TRAN,LABEL
}

# jq prelude normalising old/new lsblk JSON (strings vs numbers/bools,
# `mountpoint` vs `mountpoints`).
_JQ_LIB='
def b: if type == "boolean" then . elif . == null then false else (tostring == "1" or tostring == "true") end;
def mounts: ((.mountpoints // [.mountpoint]) | map(select(. != null)));
def allmounts: [.. | objects | select(has("name")) | mounts[]];
def sysmount: . == "/" or . == "/boot" or . == "/boot/efi" or . == "/boot/firmware" or . == "[SWAP]";
'

# Field separator for the records below. Not a whitespace character, so
# `read` keeps empty fields (a tab IFS would collapse them).
FS_SEP=$'\x1f'

# Records: PATH SIZE_BYTES ROTATIONAL MODEL TRANSPORT CURRENT_MOUNTS
# Disks that could serve as the storage tier: not holding the OS, >= 8 GB,
# not removable, not virtual.
storage_candidates() {
  lsblk_json | jq -r "$_JQ_LIB"'
    .blockdevices[]
    | select(.type == "disk")
    | select(.name | test("^(zram|loop|sr|fd|ram)") | not)
    | select((.rm | b) | not)
    | select((.size | tonumber) >= 8000000000)
    | select([allmounts[] | select(sysmount)] | length == 0)
    | [.path // ("/dev/" + .name), (.size | tonumber | tostring), (if (.rota | b) then "hdd" else "ssd" end),
       ((.model // "") | gsub("^\\s+|\\s+$"; "")), (.tran // ""), (allmounts | join(","))]
    | join("\u001f")'
}

# Given a disk path, print the block device we will mount, as a record of
#   PATH FSTYPE UUID MOUNTS KIND
# KIND is "disk" (no partitions) or "part" (largest partition).
storage_target() {
  local disk=$1
  lsblk_json | jq -r --arg d "$disk" "$_JQ_LIB"'
    .blockdevices[] | recurse(.children[]?) | select((.path // ("/dev/" + .name)) == $d)
    | if ((.children // []) | length) > 0
      then (.children | max_by(.size | tonumber)) + {kind: "part"}
      else . + {kind: (if .type == "part" then "part" else "disk" end)} end
    | [.path // ("/dev/" + .name), (.fstype // ""), (.uuid // ""), (mounts | join(",")), .kind]
    | join("\u001f")'
}

human_size() { numfmt --to=iec --suffix=B "$1" 2>/dev/null || echo "${1}B"; }

partition_path() { # partition_path /dev/sdb 1 -> /dev/sdb1 ; /dev/nvme0n1 1 -> /dev/nvme0n1p1
  if [[ "$1" =~ [0-9]$ ]]; then echo "${1}p$2"; else echo "$1$2"; fi
}

# fstab_set MOUNTPOINT LINE — replace any existing entry for MOUNTPOINT.
fstab_set() {
  local mnt=$1 line=$2 tmp
  tmp=$(mktemp)
  awk -v m="$mnt" -v mark="$FSTAB_MARK" '
    $0 == mark" "m { skip = 1; next }
    skip { skip = 0; next }
    !/^[[:space:]]*#/ && $2 == m { next }
    { print }' "$FSTAB_FILE" >"$tmp"
  printf '%s %s\n%s\n' "$FSTAB_MARK" "$mnt" "$line" >>"$tmp"
  if cmp -s "$tmp" "$FSTAB_FILE"; then rm -f "$tmp"; return 1; fi
  if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then info "[dry-run] fstab: $line"; rm -f "$tmp"; return 0; fi
  [[ -f "$FSTAB_FILE.vercelify.bak" ]] || cp -p "$FSTAB_FILE" "$FSTAB_FILE.vercelify.bak"
  cat "$tmp" >"$FSTAB_FILE"
  rm -f "$tmp"
}

fstab_storage_line() { # UUID FSTYPE
  local opts="defaults,nofail,noatime,x-systemd.device-timeout=10s"
  [[ "$2" == btrfs ]] && opts="defaults,nofail,noatime,compress=zstd:1,x-systemd.device-timeout=10s"
  printf 'UUID=%s %s %s %s 0 2' "$1" "$STORAGE_MOUNT" "$2" "$opts"
}

# Prepare the directory layout on the storage tier.
storage_layout() {
  run_cmd mkdir -p "$STORAGE_MOUNT/volumes" "$STORAGE_MOUNT/backups"
  run_cmd chmod 0755 "$STORAGE_MOUNT" "$STORAGE_MOUNT/volumes"
}

# Make the bare mount point immutable so that, if the HDD is missing at boot,
# containers fail loudly instead of silently writing databases to the SSD.
guard_mountpoint() {
  have chattr || return 0
  mountpoint -q "$STORAGE_MOUNT" && return 0
  if [[ -z "$(ls -A "$STORAGE_MOUNT" 2>/dev/null)" ]]; then
    run_cmd chattr +i "$STORAGE_MOUNT" 2>/dev/null && info "guarded empty $STORAGE_MOUNT (chattr +i)" || true
  fi
}

# Filesystem type / UUID straight from the device (blkid -p reads the disk
# itself). lsblk gets these from udev, which may be missing or stale — never
# base a destructive decision on it alone.
probe_fs()   { [[ -b "$1" ]] && blkid -p -o value -s TYPE "$1" 2>/dev/null; return 0; }
probe_uuid() { [[ -b "$1" ]] && blkid -p -o value -s UUID "$1" 2>/dev/null; return 0; }
probe_pttype() { [[ -b "$1" ]] && blkid -p -o value -s PTTYPE "$1" 2>/dev/null; return 0; }

settle() { have udevadm && udevadm settle 2>/dev/null; return 0; }

# Erasing a disk always needs an explicit yes: --format-storage, or typing the
# device name at an interactive prompt. `-y` never implies it.
confirm_erase() { # confirm_erase DEVICE REASON
  local dev=$1 reason=$2 reply=''
  has_tty || return 1
  printf '\n  %s%s%s\n' "$C_RED" "$reason" "$C_RESET" >/dev/tty
  printf '  %sALL DATA ON %s WILL BE ERASED.%s Type the device name (%s) to continue: ' "$C_BOLD" "$dev" "$C_RESET" "$dev" >/dev/tty
  IFS= read -r reply </dev/tty || true
  [[ "$reply" == "$dev" ]]
}

wait_for_node() { # wait_for_node PATH — partition nodes appear asynchronously
  local i
  for i in $(seq 1 20); do
    [[ -b "$1" ]] && return 0
    (( i == 5 )) && have partx && partx -u "${1%p1}" 2>/dev/null
    sleep 0.5
  done
  return 1
}

format_device() { # format_device DISK KIND TARGET -> prints the formatted device
  local disk=$1 kind=$2 target=$3 dev
  if [[ "$kind" == disk ]]; then
    run_cmd wipefs -a "$disk" >&2
    run_cmd parted -s "$disk" mklabel gpt mkpart "$STORAGE_LABEL" ext4 1MiB 100% >&2
    settle
    have partx && run_cmd partx -u "$disk" >&2 2>/dev/null
    dev=$(partition_path "$disk" 1)
    if [[ "$VERCELIFY_DRY_RUN" != 1 ]] && ! wait_for_node "$dev"; then
      # No udev to create the node (minimal containers, odd kernels): use the whole disk.
      warn "partition node $dev did not appear — formatting the whole disk instead"
      run_cmd wipefs -a "$disk" >&2
      dev=$disk
    fi
  else
    dev=$target
    run_cmd wipefs -a "$dev" >&2
  fi
  # -m 1: keep 1% reserved (not 5%) — this disk is for data, not the OS.
  run_cmd mkfs.ext4 -F -q -L "$STORAGE_LABEL" -m 1 "$dev" >&2
  settle
  echo "$dev"
}

# setup_storage DEVICE FORMAT(0|1)
#   DEVICE "auto" picks the only candidate disk, "none" skips.
setup_storage() {
  local want=${1:-auto} allow_format=${2:-0}
  step "Storage tier (${STORAGE_MOUNT})"

  if mountpoint -q "$STORAGE_MOUNT" 2>/dev/null; then
    ok "$STORAGE_MOUNT is already mounted ($(findmnt -no SOURCE "$STORAGE_MOUNT"))"
    storage_layout
    return 0
  fi

  local disk=''
  if [[ "$want" == none ]]; then
    warn "storage disk skipped — everything will live on the system disk"
  elif [[ "$want" != auto ]]; then
    disk=$want
    [[ -b "$disk" || -n "$LSBLK_JSON" ]] || die "$disk is not a block device"
  else
    local cands=() line
    while IFS= read -r line; do [[ -n "$line" ]] && cands+=("$line"); done < <(storage_candidates)
    if (( ${#cands[@]} == 0 )); then
      warn "no second disk found — single-disk mode (HDD tier paths live on the SSD)"
    elif (( ${#cands[@]} == 1 )); then
      disk=${cands[0]%%"$FS_SEP"*}
    else
      info "several candidate disks found:"
      local i=1 c
      for c in "${cands[@]}"; do
        IFS=$FS_SEP read -r p s r m t _ <<<"$c"
        printf '      %d) %s  %s  %s %s %s\n' "$i" "$p" "$(human_size "$s")" "$r" "$m" "$t" >&2
        i=$((i + 1))
      done
      local pick
      ask pick "Which disk should hold databases/volumes? (number, or 'none')" "none"
      if [[ "$pick" =~ ^[0-9]+$ ]] && (( pick >= 1 && pick <= ${#cands[@]} )); then
        disk=${cands[pick - 1]%%"$FS_SEP"*}
      fi
    fi
  fi

  if [[ -z "$disk" ]]; then
    run_cmd mkdir -p "$STORAGE_MOUNT"
    storage_layout
    return 0
  fi

  local target fstype uuid mounts kind probed
  IFS=$FS_SEP read -r target fstype uuid mounts kind < <(storage_target "$disk")
  [[ -n "$target" ]] || die "could not inspect $disk"
  # lsblk can list stale or phantom partitions (no udev, reused loop/USB
  # devices). The device itself is the truth: a filesystem directly on the
  # disk with no partition table, or a partition node that doesn't exist,
  # means "use the whole disk".
  if [[ "$kind" == part && -b "$disk" ]]; then
    local whole_fs
    whole_fs=$(probe_fs "$disk")
    if { [[ -n "$whole_fs" ]] && [[ -z "$(probe_pttype "$disk")" ]]; } || [[ ! -b "$target" ]]; then
      target=$disk kind=disk fstype=$whole_fs uuid=$(probe_uuid "$disk")
      mounts=$(findmnt -rno TARGET -S "$disk" 2>/dev/null | paste -sd, - || true)
    fi
  fi
  probed=$(probe_fs "$target")
  [[ -n "$probed" ]] && fstype=$probed
  if [[ -z "$fstype" && "$kind" == disk && -n "$(probe_pttype "$disk")" ]]; then
    die "$disk has a partition table but no usable partition — partition it yourself, or pass --storage-device /dev/<partition>"
  fi
  info "storage disk: $disk → $target (${fstype:-no filesystem})"

  if [[ -n "$mounts" ]]; then
    die "$target is mounted at $mounts — unmount it first (sudo umount $mounts) or pick another disk"
  fi

  case "$fstype" in
    ext4|xfs|btrfs) ;;
    "")
      if [[ "$allow_format" != 1 ]] && ! confirm_erase "$disk" "$disk has no filesystem and must be formatted (ext4)."; then
        die "refusing to format $disk without explicit consent (re-run with --format-storage to erase it)"
      fi
      target=$(format_device "$disk" "$kind" "$target")
      fstype=ext4 ;;
    *)
      if [[ "$allow_format" != 1 ]] && ! confirm_erase "$target" "$target is $fstype, which has no Unix permissions (bad for databases)."; then
        die "$target uses $fstype; re-run with --format-storage to reformat it as ext4 (erases it)"
      fi
      target=$(format_device "$disk" "$kind" "$target")
      fstype=ext4 ;;
  esac

  if [[ "$VERCELIFY_DRY_RUN" == 1 ]]; then
    uuid=${uuid:-DRY-RUN-UUID}
  else
    uuid=$(probe_uuid "$target")
  fi
  [[ -n "$uuid" ]] || die "could not read the UUID of $target"

  run_cmd mkdir -p "$STORAGE_MOUNT"
  guard_mountpoint
  fstab_set "$STORAGE_MOUNT" "$(fstab_storage_line "$uuid" "$fstype")" && info "updated $FSTAB_FILE (nofail)"
  run_cmd systemctl daemon-reload
  run_cmd mount "$STORAGE_MOUNT" || die "mounting $STORAGE_MOUNT failed — check 'dmesg | tail'"
  if [[ "$VERCELIFY_DRY_RUN" != 1 ]]; then
    # systemd unmounts fstab mounts whose device unit it cannot see (no udev).
    sleep 1
    mountpoint -q "$STORAGE_MOUNT" ||
      die "$STORAGE_MOUNT did not stay mounted — see 'systemctl status $(systemd-escape --path --suffix=mount "$STORAGE_MOUNT")' (is udev running?)"
  fi
  storage_layout
  ok "mounted $target at $STORAGE_MOUNT ($(df -h --output=size "$STORAGE_MOUNT" 2>/dev/null | tail -1 | tr -d ' ') total)"
}
