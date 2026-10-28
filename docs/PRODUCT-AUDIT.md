# Product and installation audit — 29 October 2026

## Findings fixed in this change

| Priority | Trigger and impact | Correction |
| --- | --- | --- |
| P1 | Edge installation uses scripts from main but downloads binaries using Cargo.toml's version. A release with that version can silently supply older daemons with a different contract. | Edge/main installs build their daemons from the downloaded source; explicit binaries and bundled artifacts remain supported. Regression test checks that no release download happens. |
| P1 | Rerun the server installer directly on a worker. Defaults overwrite its saved role with main, potentially initializing coordinator/platform behavior. The normal `vercelify update` command already passes `--worker`. | Recover the saved role even when there is no domain, preserving an explicit worker flag. |
| P2 | Follow setup's advice to choose `/dev/<partition>`. Storage lookup searches only top-level disks and cannot inspect the partition. | Walk the lsblk tree and preserve partition identity, so formatting a partition does not treat it as a whole disk. |
| P2 | Request optional Mac display tools and Homebrew fails. ffmpeg failure can abort setup; Moonlight failure still prints that everything is ready. | Warn with retry commands, continue installing the app, and print readiness only when both install steps succeed. |
| P2 | Streaming an existing Mac screen is presented as automatically creating an extended desktop. | Label it screen sharing and explain that extension requires a virtual display or adapter. Prevent starting with missing capture prerequisites. |
| P2 | Remote desktop instructions say Sunshine supplies the PIN. | Explain that Moonlight shows the PIN and the user submits it in Sunshine. |
| P2 | Successful setup prints the mesh IP twice in the Network line. | Print one address. |

## Product changes to pursue next

These are design recommendations and hardware-dependent gaps, not claims that a replacement streaming system has shipped.

1. **Make installation finish with a readiness result.** Today mesh initialization/join errors warn and setup can still print “ready.” Report agent, network, storage and optional display readiness separately, with one retry action for each. Do not make optional streaming a prerequisite for deploying apps.
2. **Use a short default onboarding path.** Install server → choose discovered server → enter code. Keep domains, remote access, AI downloads and display configuration as later choices. The existing wizard asks about several optional products before the core installation finishes.
3. **Separate “Mirror my Mac,” “Extend my Mac,” and “Control Linux.”** Mirroring can use the existing capture pipeline. Extension needs an actual additional macOS display; changing codecs cannot create one. An integrated virtual-display implementation requires a separate compatibility and permission investigation before promising automatic extension.
4. **Add display preflight with actionable recovery.** Check capture permission, screen enumeration, ffmpeg, mpv, Linux session/backend and the mesh path. The default receiver uses DRM when no display environment is configured. On a running Linux desktop this may conflict with the compositor; current documentation requires manual agent.toml edits. Automatic session-aware launching needs real X11 and Wayland tests and should avoid guessing XAUTHORITY or granting broad desktop access.
5. **Guide Sunshine pairing inside the app.** Show “Open Sunshine setup” after establishing forwards, then explain the PIN step. The current tools still require Homebrew, Moonlight and Sunshine setup. Keep this as an optional remote-control flow. Avoid adding another server/streaming stack until capture/session setup is solved.
6. **Measure stream health.** A running ffmpeg process plus a running mpv process is currently labeled “Live”; neither proves decoded frames reach the panel. Add first-frame confirmation and loss/latency feedback before treating process status as playback health.

## Evidence and limits

Reviewed bootstrap and Mac installation, host setup/storage/update/desktop helpers, pairing and mesh forwarding, receiver and sender lifecycle, onboarding, polling and server-switch boundaries. Existing automated suites exercise the broader platform, storage, databases, AI, backup, power guard, domains, routing, server management and navigation. This is a source and automated-test sweep; it does not certify every feature on physical hardware.

The full Rust workspace command passes on this Mac. Docker integration test entrypoints return without exercising Docker unless `VERCELIFY_DOCKER_TESTS=1` is set; their green result is not deployment evidence. Live Linux installation, systemd/firewall/network-namespace contracts, GPU output, physical mirroring, Screen Recording permission prompts and actual Sunshine pairing still need Linux/Mac hardware validation.

The broad host Bats run encounters macOS incompatibilities (`stat -c`, GNU `sed -i`, `touch -d`) and Linux kernel assumptions. Those failures must be rechecked on supported Linux rather than interpreted as confirmed product defects. Targeted installer regression suites run locally.

## Research references

- [mpv reference manual](https://mpv.io/manual/stable/): explicit GPU contexts differ between DRM, X11 and Wayland. This supports treating receiver session/backend setup as a distinct requirement.
- [Sunshine getting started](https://docs.lizardbyte.dev/projects/sunshine/master/md_docs_2getting__started.html): web setup and the PIN submitted after Moonlight requests pairing.
- [Moonlight setup guide](https://github.com/moonlight-stream/moonlight-docs/wiki/Setup-Guide): the PIN is displayed by Moonlight and entered on the host.
