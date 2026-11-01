# Audit evidence — 1 November 2026

See [the full report](../../REPOSITORY-AUDIT-2026-11-01.md) for priorities,
scores, impact, fixes and verification limits. `summary.json` records counts.

The logs are fresh results from commit `6407cf7` with the pre-existing local
work preserved. `film.log` concerns the untracked local `launch-film` project.
Eight Rust tests were ignored; a green log does not mean those tests ran.

## Isolated platform probes

From the repository root:

```bash
python3 docs/audits/2026-11-01/reproduce-platform.py
```

Requires Rust with cached repository dependencies, Docker Compose (standalone
`docker-compose` or the Docker CLI plugin), and working DNS for the loopback
wildcard-name probe. Only `compose config` is invoked against the real Compose
binary: no images are pulled and no application containers start. The other
Docker operations are stubs. All credentials, victim files and state are synthetic
and temporary. Build output uses the repository's existing `target` directory.

The runner compiles the actual platform/mesh libraries. The rewrite-origin probe
compiles the exact `private_origin` function extracted from `agent/src/project.rs`.
It tests validator acceptance and DNS resolution; it does not publish a route.

Expected observations are recorded in `repros.log`: accepted outside-file write,
accepted outside-checkout context, retained Compose privileges/ports, a bind source
inside the checkout, wrong default service, missing Compose autodetection, a failed
volume deletion returned as success, 404 readiness acceptance, changed environment
value, and collisions from concurrent private-file writes. Race failure counts
and temporary paths vary. A final valid JSON document does not rule out the race.

## Isolated backup probes

`backup-probes.sh` must run in a disposable Linux container with bash, rsync,
coreutils and jq. The `vercelify-dev-linux` image is created by `./dev check host`
on a Mac, or can be built from the definition in `dev`.

From the repository root, with that image available:

```bash
docker run --rm -i -v "$PWD":/src:ro vercelify-dev-linux bash -s \
  < docs/audits/2026-11-01/backup-probes.sh
```

The script sources the real backup functions. Systemctl/Docker calls are shell
stubs, restic is `/usr/bin/true`, and file installation/rsync target a temporary
directory inside the container. A snapshot fixture is pre-populated. It cannot
access the Docker socket and never stops a real container.

`backup-repro.log` records successful restore despite a failed stop, a surviving
stale file, an overwritten engine-state marker, omitted old device-token markers,
and lost S3 credentials/custom retention after setup. This proves orchestration
and filesystem behavior, not real database corruption or restic restore integrity.

## Suite logs

- `rust.log`, `agent-desktop.log`: Rust test results and ignored-test notices.
- `cockpit-test.log`, `e2e.log`: frontend unit tests and browser demo tests.
- `host.log`, `shellcheck.log`, `fmt.log`, `clippy.log`: host/lint checks; empty
  standalone lint logs mean the command completed without diagnostics.
- `cockpit-build.log`, `website-build.log`, `video.log`, `film.log`: build/check results.
- `*-dependencies.json`: npm advisory metadata; see the report's reachability triage.

Product source is unchanged. These files are audit material, not implemented fixes.
