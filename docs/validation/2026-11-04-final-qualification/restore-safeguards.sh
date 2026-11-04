#!/usr/bin/env bash
# Real Docker/restic/rsync, private fixture paths. systemctl is an explicit
# process-control shim in this non-systemd container; no systemd claim is made.
set -euo pipefail
[[ ! -d /etc/vercelify ]]
T=/var/lib/final-restore-safeguards
[[ ! -e $T ]]
mkdir -p "$T"/{bin,etc,state/backup-stage,storage/volumes}
export VERCELIFY_ETC=$T/etc VERCELIFY_STATE=$T/state VERCELIFY_LOG=$T/trace.log
export STORAGE_MOUNT=$T/storage ASSUME_YES=1 ROLE=main NO_COLOR=1
export DOCKER_VOLUMES_DIR=/var/lib/docker/volumes
export RESTIC_PASSWORD=qualification-fixture-password RESTIC_REPOSITORY=$T/repository
printf "RESTIC_PASSWORD='%s'\nBACKUP_REPOS='%s'\n" "$RESTIC_PASSWORD" "$RESTIC_REPOSITORY" > "$T/etc/backup.env"
printf snapshot-control > "$T/state/backup-stage/platform.db"
printf snapshot-key > "$T/state/master.key"
printf snapshot-tokens > "$T/state/tokens.json"
printf snapshot-agent-token > "$T/etc/agent.token"
printf current-control > "$T/state/platform.db"
real_docker=$(command -v docker)
cat > "$T/bin/docker" <<EOF
#!/bin/sh
if [ -e '$T/inspect-fails' ] && [ "\$1" = ps ]; then exit 1; fi
if [ -e '$T/stop-fails' ] && [ "\$1" = stop ]; then exit 1; fi
exec '$real_docker' "\$@"
EOF
cat > "$T/bin/systemctl" <<EOF
#!/bin/sh
case "\$1 \$2" in
 'stop vercelify-agent') kill "\$(cat '$T/service.pid')"; rm -f '$T/service.pid';;
 'start vercelify-agent') sleep 3600 & echo \$! > '$T/service.pid';;
esac
EOF
chmod +x "$T/bin/"*
export PATH=$T/bin:$PATH
systemctl start vercelify-agent
# Only explicitly named disposable volumes and writers belong to this contract.
docker volume create vfy-finalq-restore >/dev/null
docker volume create unrelated-finalq-restore >/dev/null
live=$DOCKER_VOLUMES_DIR/vfy-finalq-restore/_data
other=$DOCKER_VOLUMES_DIR/unrelated-finalq-restore/_data
printf snapshot-data > "$live/kept"
printf unrelated-snapshot > "$other/kept"
restic init -q
restic backup -q "$T/etc" "$T/state/backup-stage" "$T/state/master.key" "$T/state/tokens.json" "$DOCKER_VOLUMES_DIR/vfy-finalq-restore" "$DOCKER_VOLUMES_DIR/unrelated-finalq-restore"
printf newer-data > "$live/kept"; printf newer-only > "$live/newer-only"
printf unrelated-current > "$other/kept"
docker run -d --name vfy-finalq-writer --label vercelify.managed=platform -v vfy-finalq-restore:/data alpine:3.20 sleep 3600 >/dev/null
docker run -d --name unrelated-finalq-writer -v unrelated-finalq-restore:/data alpine:3.20 sleep 3600 >/dev/null
source /workspace/host/lib/common.sh
source /workspace/host/lib/backup.sh
# Inspection/stop failures must leave live control state and volume bytes intact.
for failure in inspect-fails stop-fails; do
 touch "$T/$failure"
 if (backup_restore latest --instance --target "$T/restore-$failure"); then echo 'unexpected restore success' >&2; exit 1; fi
 rm "$T/$failure"
 [[ $(cat "$T/state/platform.db") == current-control ]]
 [[ $(cat "$live/kept") == newer-data && -f $live/newer-only ]]
 kill -0 "$(cat "$T/service.pid")"
 [[ $(docker inspect -f '{{.State.Running}}' vfy-finalq-writer) == true ]]
 echo "PASS: $failure aborts before live replacement; writer and service remain available"
done
backup_restore latest --instance --target "$T/restored"
[[ $(cat "$T/state/platform.db") == snapshot-control ]]
[[ $(cat "$live/kept") == snapshot-data && ! -e $live/newer-only ]]
[[ $(cat "$other/kept") == unrelated-current ]]
[[ $(cat "$T/state/tokens.json") == snapshot-tokens && $(cat "$T/etc/agent.token") == snapshot-agent-token ]]
[[ $(docker inspect -f '{{.State.Running}}' vfy-finalq-writer) == true ]]
[[ $(docker inspect -f '{{.RestartCount}}' unrelated-finalq-writer) == 0 ]]
kill -0 "$(cat "$T/service.pid")"
[[ $(find "$DOCKER_VOLUMES_DIR/vfy-finalq-restore" -name newer-only | wc -l) -eq 1 ]]
echo 'PASS: exact snapshot tree replaces newer files; prior tree retained; tokens restored; unrelated volume/writer preserved; owned writer resumes'
docker rm -f vfy-finalq-writer unrelated-finalq-writer >/dev/null
docker volume rm vfy-finalq-restore unrelated-finalq-restore >/dev/null
systemctl stop vercelify-agent
