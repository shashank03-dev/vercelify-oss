# First owned-hardware pilot

Use trusted synthetic data and a host you can recover locally. Start with the
[qualified configurations](../RELEASE-QUALIFICATION.md); leave the battery policy
off until ordinary deploy/recovery works. This guide is an operator procedure, not
a claim that every step has been observed with an independent pilot.

1. Install and pair using the repository's [platform guide](../PLATFORM.md). Run
   Doctor and resolve missing Docker/resource/isolation prerequisites. Record the
   actual machine, agent commit and Docker/OS versions.
2. In **Apps → New app**, choose an image app named `pilot-web` using
   `traefik/whoami:v1.11.0`, explicit port 80, 64 MiB and one CPU. This is a small
   routing fixture; it does not represent a production Next.js app or database.
   Deploy and verify it from the intended client network. Record its route and ID.
3. For your real application, use a supported Git build configuration/Dockerfile,
   or [build on your computer/CI](../PLATFORM.md#build-on-your-computer-or-in-ci).
   Declare its architecture, resources, health path and persistent volumes. Repeat
   deployment and rollback while probing from outside the host. Keep logs/denominators.
4. If the app uses PostgreSQL previews, enable the fresh PostgreSQL 17 preview
   service, a migration command and a synthetic seed command. Keep previews private.
   Do not share production `DATABASE_URL`, credentials, volumes or copied data.
   Use the [preview database instructions](../PLATFORM.md) for runtime-only references.
5. Back up a disposable PostgreSQL database to an independently retained encrypted
   repository. Save the recovery artifact privately. Run a restore drill for a known
   snapshot with an application-specific read assertion, then exercise recovery
   without the original source/control key. A success label without those known
   contents is insufficient. Use [backup guidance](../BACKUPS.md).
6. To invite a recipient outside the mesh, configure separate guest DNS/HTTPS,
   choose **Share**, create a short one-session invitation and transmit its complete
   secret privately. Verify expiry/revocation from that recipient's actual network.
   Do not post invitations to public PR comments. Public Internet/tunnel qualification
   is separate from the lab's local TLS/browser evidence.
7. Only after ordinary recovery works, explicitly select noncritical apps with Sleep,
   enable battery conservation and complete the [physical power matrix](../POWER-RESILIENCE.md#physical-qualification).
   Keep a critical app outside Sleep, while acknowledging it still stops at shutdown.
   Include router/ISP loss and independent backup recovery in the record.
8. Export recovery settings, record resource/volume ownership and document how to
   retire the pilot. Removing an app does not substitute for independently retaining
   important data. Record first deploy and a second deploy on another day, assistance
   needed, failures and workarounds before claiming pilot activation or repeat use.
