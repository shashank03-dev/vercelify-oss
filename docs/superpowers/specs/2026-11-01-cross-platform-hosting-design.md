# Cross-platform hosting and the Vercelify environment

Date: 2026-11-01
Status: implemented as amended by `2026-11-01-cross-platform-hosting-review.md` (see its §6 for what is built and verified); binding design in `docs/design/SYSTEM-DESIGN.md` §19.

## 1. Product contract

Vercelify Desktop on Windows, macOS, or Linux manages the owner's deliberately connected computers. The owner develops with their usual tools and chooses where a project runs. Closing Desktop does not stop hosting. This is a full product change, with platform qualification and regression checks, rather than a demonstration.

Each non-Linux host has exactly one active, Vercelify-owned Linux hosting environment shared by all its apps, databases, builds, and supporting services. Each Linux host uses the existing Linux installation with no additional virtual machine. Adding a project must never create another Linux environment. Individual apps still have separate containers, networks, secrets, and data volumes inside that shared environment.

"One" means one environment per physical host, not one environment stretched across multiple computers. Multiple hosting computers remain independent machines connected through Vercelify mesh. Installation and repair use a machine-wide ownership lock so two desktop clients or OS accounts cannot create duplicate managed environments.

At the first deployment to an unprepared non-Linux host, Desktop checks that host, recommends resources, lets the owner change them, and asks to **Create Vercelify environment**. Creation begins only after agreement. Subsequent projects reuse it. Linux skips this creation flow.

Normal screens use **Vercelify environment**, **Memory**, **Processing power**, **Storage limit**, **Recommended for this computer**, and **Create environment and deploy**. Explain what resources and storage will be used; technical implementation details belong in optional diagnostics and documentation. Do not conceal system permission requests, installation changes, downtime, or limitations.

## 2. Scope and boundaries

Included: desktop management on all three OS families; host enrollment before an environment exists; one environment per non-Linux host; remote provisioning, lifecycle and resource controls; mesh integration; existing hosting features; safe machine/app/deployment removal; persistence and recovery; platform installers and updates; security, resource and cross-platform tests.

Not included: a code editor, strangers submitting workloads, a public compute marketplace, Kubernetes, arbitrary native Windows/macOS application hosting, automatic database migration between hosts, or a claim of identical GPU/display/hardware features on every OS. Existing screen-streaming functionality must continue working on its currently supported combinations; new combinations advertise capabilities explicitly.

All hosting goes through the existing Linux deployment engine, including inside the environment on Windows/macOS. The previous native-per-project execution proposal is superseded by the owner's shared-environment decision.

## 3. Existing architecture and reuse

Relevant inspected sources:

- `docs/design/SYSTEM-DESIGN.md`, `docs/ARCHITECTURE.md`: platform database, jobs, reconciliation, networking and failure contracts.
- `cockpit/src/views/ServersView.tsx`, `cockpit/src/lib/api.ts`, `cockpit/src-tauri/src/lib.rs`: multiple saved servers and local disconnect.
- `agent/src/platform_api.rs`, `crates/vercelify-platform/src/nodes.rs`, `cockpit/src/components/PlatformServers.tsx`: worker removal, with dependent-workload checks.
- `agent/src/mesh.rs`, `crates/vercelify-platform/src/mesh_registry.rs`: network revocation is separate from worker removal.
- `crates/vercelify-platform/src/placement.rs`, `agent/src/worker.rs`: placement, heartbeat, runtime and build restrictions.
- `crates/vercelify-mesh/src/lib.rs`, `cockpit/src-tauri/src/mesh.rs`: Linux/macOS daemon assumptions; Windows remains engineering work.
- `crates/vercelify-platform/src/deployments.rs`, `agent/src/platform_api.rs`: deployment listing, inspection and cancellation; no dedicated individual-deployment deletion route found.
- `.github/workflows/ci.yml`, `.github/workflows/release.yml`: existing checks and artifacts; these do not establish all nine controller/host combinations.

Keep the Rust Linux agent, SQLite store, deployment engine, Docker/BuildKit, proxy, backups, shared wire types, React/Tauri UI and existing mesh. Add a native host manager rather than porting Linux service/storage code to Windows/macOS. Existing unrelated checkout changes are outside this work.

## 4. Approaches considered

| Approach | Benefit | Cost / reason |
| --- | --- | --- |
| Native execution of each project on each OS | No Linux guest overhead | Different hosting behavior, dependency compatibility and isolation across systems; superseded by the owner's decision |
| One Linux environment per project | Straightforward separation | Repeated system overhead and lifecycle complexity; explicitly rejected by the owner |
| One shared Vercelify environment per non-Linux host | Reuses the hosting engine and shares system overhead | Requires a native host manager, measured resource policy and qualified VM providers; selected by the owner |

## 5. Components and authority

Desktop is the interface, not the always-running authority. The existing designated main server remains the source of desired hosting state. Each host manager remains authoritative for its local resource grant, environment identity, installation permissions and actual lifecycle state. Desktop never stores the only copy of an in-progress operation.

A new small Rust host-manager service runs directly on Windows/macOS before any guest exists. It reports physical-host capabilities, joins mesh, accepts owner-authorized structured operations, creates/manages the single guest, records jobs and survives desktop exits. Privileged operations use a narrowly scoped local helper. The service does not expose an unrestricted command runner.

The Linux guest runs the existing agent and hosting stack. Main-server roles can live inside a Mac/Windows host's environment; Linux main servers continue running directly. Host-manager APIs stay reachable independently of the guest so a stopped or broken environment is repairable remotely.

For a brand-new installation without a main server, the first paired desktop and native host use a direct bootstrap mesh: the initial installer creates a host-owned trust root and one-time pairing claim, and the desktop runs the existing userspace mesh against that native control endpoint. Pairing is encrypted/authenticated before any resource inspection or mutation. After the first environment starts, a bounded, journaled handoff enrolls its hosting agent in the same trust domain and establishes the designated main server. No second public management interface is left behind. Existing main servers enroll new native host managers through scoped invitations. Bootstrap coordinator extraction from the Linux-only agent is explicit implementation work, not assumed available today.

Each physical host and its internal hosting agent have separate bound identities, presented as one machine in Desktop. Delegation is scoped to that host and assigned workloads. Offline operation uses last accepted state with bounded credential validity; network revocation and credential expiry limitations must be documented. Restoring an image must not resurrect old host identities.

## 6. Provider selection and qualification

Implementation proposal, not an already validated platform guarantee:

- Linux: existing native engine, with a Vercelify-owned aggregate resource group.
- macOS: pinned Lima with Apple's virtualization backend, custom configuration, no default host-home mounts, dedicated Vercelify storage, and boot service installed with explicit initial authorization.
- Windows: evaluate a dedicated QEMU/WHPX guest first because its configuration is independently owned. Pin and package the provider, firmware and guest image; verify required Windows editions/builds, acceleration, networking, boot-service identity and resource accounting before enabling hosting. No slow software-emulation fallback without an explicit experimental mode.
- WSL2 is a candidate only if it can meet the same non-interference and resource contracts. Do not edit global `.wslconfig`, run global `wsl --shutdown`, or alter existing distributions to impose Vercelify limits. Guest-only workload limits do not prove a total physical-host limit. Do not ship Lima's experimental Windows drivers as an assumed production solution.

Provider capability tests must pass before supported-platform claims. Windows ARM and old OS releases remain unqualified until tested; architecture and OS minimums are published explicitly. Desktop-only support may be broader than hosting support. Unsupported hosts can still manage qualified hosts and receive a clear reason they cannot host yet.

## 7. Installation and first-deployment flow

1. On the host, install the native manager and pair once. Setup checks hardware acceleration, OS features, disk access, service installation and reboot requirements. It obtains necessary local permissions before advertising remotely provisionable status. No full environment is created yet.
2. From any supported Desktop, choose the host and project. Query authenticated physical resources and prerequisite status. For a prepared Linux host, continue through existing deployment. For a non-Linux host already ready, reuse its environment.
3. For an unprepared non-Linux host, show a resource recommendation based on fresh physical-host measurements and provider/image requirements. The owner can adjust validated values or cancel. Nothing is provisioned on cancellation.
4. **Create environment and deploy** submits a durable idempotent operation with an expected configuration revision, accepted recommendation and deployment request. The host revalidates capacity before mutation; changed measurements produce a refreshed proposal when the accepted grant cannot be honored.
5. Show understandable stages: Checking computer, Preparing environment, Starting hosting, Checking connection, Deploying your app. Refreshing Desktop or opening another supported desktop resumes the same job.
6. Mark ready only after guest boot, authentication, resource enforcement, persistent storage, routing and agent health checks pass. Dispatch the pending deployment once. Failed or interrupted setup retains explicit retry/cleanup state and cannot start duplicate environments or deployments.

Later memory/CPU/storage changes happen in the machine's Environment settings. Explain whether a restart is needed and obtain agreement before interrupting apps. Never promise all resource changes are live. Disk expansion is supported when safe; disk shrink is disabled until a verified migration process exists.

## 8. Resource policy

Use physical-host measurements, not guest-reported capacity, for recommendations: installed and currently available RAM, logical CPUs, OS/build/architecture, free space on the chosen disk, existing workloads, power state, and virtualization readiness. Missing measurements mean unavailable recommendations, not fabricated zeros/defaults.

Recommendations are conservative estimates, never called perfect. A versioned policy reserves resources for the owner's OS and usage, checks measured guest overhead and minimum workload headroom, and refuses environments too small to function safely. The exact formula and minimums are chosen from benchmarks, with boundary-case tests, before shipping.

The accepted grant covers the complete environment: guest system, engine/proxy, builds, apps, databases, caches, logs and backups stored there. Show the native manager/provider overhead separately and account for it in safety headroom. CPU count is not a hard CPU-percentage guarantee: enforce and measure the promised whole-provider processing limit with OS controls. Memory accounting includes provider overhead; use a measured provider allowance rather than killing the VM at its nominal guest-memory setting. Clearly distinguish RAM, disk capacity, current disk usage and runtime CPU consumption.

Enforce an aggregate guest workload budget below the environment ceiling plus per-app limits. Build admission reserves memory/CPU/storage atomically, includes image-pull and temporary-write peaks, and releases reservations after crashes. The scheduler uses granted remaining capacity, not all physical capacity or the existing optimistic unknown-main-server scoring path. Serialize/reserve concurrent work to prevent two deployments both spending the same headroom. Queue builds before putting running services at risk.

Disk safety requires a bounded virtual disk, log rotation, cache retention, reserved space for recovery and monitoring of actual physical free space; a sparse disk's logical maximum is not free physical storage. Stop admitting writes/builds before the host is full. Never prune unowned files or silently delete live data.

Reducing a grant below committed workload needs requires explicit resolution. Never silently increase a grant, discard a database, or claim ordinary laptop use is unaffected. Thermal/battery pressure pauses new heavy work first. A paused, asleep or powered-off host cannot serve apps; automatic database failover is not promised.

## 9. Durable state, API and concurrency

Proposed new shared records: `HostCapabilities`, `EnvironmentRecommendation`, `EnvironmentGrant`, `ManagedEnvironment`, `HostOperation`, `HostAgentBinding`. Keep them distinct from existing project deployment `Environment` types. All units are explicit bytes, millicpus or documented vCPU counts.

Host-local state includes stable host/environment IDs, provider identity, desired/observed state, configuration revision, accepted resource grant, image digest/version, job journal, last health sample and structured failure reason. Protect secrets with OS-appropriate permissions/credential storage; Windows ACLs replace Unix mode assumptions. Persist state atomically and reconcile actual provider state after every process restart.

Proposed authenticated host-manager API namespace:

| Endpoint | Purpose |
| --- | --- |
| `GET /v1/host/capabilities` | Physical resources and prerequisites |
| `POST /v1/host/environment/recommendation` | Read-only proposal bound to current measurements and policy version |
| `GET /v1/host/environment` | Desired/observed state, grant, revision and health |
| `POST /v1/host/environment` | Owner-approved create-or-resume with idempotency key |
| `PATCH /v1/host/environment` | Validated, revision-checked resource change |
| `POST /v1/host/environment/start` | Idempotent start |
| `POST /v1/host/environment/stop` | Explicit stop with impact acknowledgement |
| `GET /v1/host/operations/{id}` | Durable progress and outcome |
| `DELETE /v1/host/environment` | Explicit destructive operation after dependency/data checks |

Lifecycle: `not_created -> preparing -> starting -> verifying -> ready`; side states `blocked`, `failed`, `stopped`, `updating`, `removing`. Each failure records the last completed stage and whether retry is safe. A per-host lease/lock and revision checks reject competing create/update/delete operations. Request replay returns the original result and operation ID. Removal tombstones prevent stale provisioning or heartbeats recreating revoked records.

API versions/capabilities allow old Linux servers to remain usable. Absence of new host capabilities means legacy behavior, not an unsupported request. New APIs are authenticated before response, including reads; owner authorization is required for environment mutation. Mesh membership alone is insufficient permission.

## 10. Networking, routing and availability

Native host-manager and guest-hosting identities use scoped mesh policies. Guest identity is bound through a one-use bootstrap credential; no long-lived owner credential is baked into an image. Management, runtime and public-app traffic have separate policies. A private mesh route must not accidentally expose the host-manager endpoint publicly.

Qualify native-to-guest reachability, guest mesh UDP/relay behavior, private `.vfy` access, HTTP/WebSocket streaming, image registry pulls, webhooks, databases and outbound public tunnels on each provider. Guest IP changes must not change the machine's public identity. Avoid public host port exposure by default and detect local port conflicts.

Running apps continue through Desktop exits and temporary coordinator outages using the existing data-path contract. New deployment and mutation require available authority. Do not claim automatic transfer of volume-backed workloads when a host fails. Main-server recovery uses existing backups/standby mechanisms, with environment-state recovery added and independently tested.

## 11. Removal semantics

| Action | Contract |
| --- | --- |
| Disconnect from Desktop | Removes that desktop's saved connection; hosting continues; does not imply token revocation |
| Revoke device access | Invalidates the selected device credentials/network permissions, with offline-expiry behavior explained |
| Remove hosting machine | Stops new placements, checks dependencies, explicitly handles worker/runtime credentials and mesh identity, and rejects unsafe removal |
| Delete old deployment | Owner-only, serialized with deploy/rollback; rejects live, active, preview-current or referenced versions; removes only unreferenced artifacts and preserves app data |
| Delete app | Existing app removal with explicit data-retention/purge choice and clear impact |
| Delete Vercelify environment | Separate destructive operation; checks all apps/databases, requires a reviewed data decision, stops the owned guest and removes only owned resources |

Deployment deletion uses a durable cleanup job/tombstone and artifact reference checks across rollbacks, replicas, shared image digests and previews. Retain a minimal audit record after cleanup. Never remove a shared registry image based only on one deleted deployment. Offline machines show incomplete cleanup honestly and cannot regain access via an old invitation. Removing the designated main server requires planned transfer/recovery, not an ordinary worker delete.

## 12. Security and non-interference

Assume trusted ownership but potentially compromised application dependencies. Each app has its own restricted runtime, resources, network and secrets. Do not expose Docker sockets, native home directories, provider sockets, host-manager credentials or mesh administrative keys to apps. No privileged containers or arbitrary host bind mounts by default. Validate path ownership, symlinks, image references, parameters and command arguments. Protect the bootstrap boundary and local privileged IPC against non-owner users.

Use signed/version-pinned provider and guest artifacts, digest verification, credential rotation, structured audit events, rate limits and redacted logs. Never remotely disable OS security or approve OS prompts on the user's behalf. The installer may request local administrative approval; hardware virtualization may require a physical firmware change and reboot. Explain these blockers before promising a remote-only first deployment. After prerequisites are satisfied, normal operation must require no host terminal or local settings work.

The shared guest is a common failure boundary. Container isolation and admission limits reduce interference but do not make kernel failures impossible. Back up persistent data with application-consistent methods; an arbitrary running-VM snapshot is not a database backup. Updates checkpoint state, retain recovery material, and preserve app data. After schema migrations, rollback restores a compatible state snapshot rather than blindly running an older binary against a newer database.

## 13. Regression-safe delivery

Implement additive contracts behind capabilities, retaining the existing native Linux path. Extract reusable mesh/bootstrap interfaces without changing Linux default behavior. UI gates only non-Linux hosts lacking a ready environment; adding the second app never triggers setup. Existing paired server settings migrate atomically with rollback copies. Upgrades must not silently move Linux data or reinitialize a host's environment.

Publish provider/OS support only after real qualification. Build desktop and native service artifacts on their respective OS runners, with signing/notarization and documented recovery/uninstall behavior. Use controlled hardware-backed runners when hosted CI cannot exercise virtualization; mock providers alone cannot certify hosting.

## 14. Test and acceptance plan

| Layer | Required evidence |
| --- | --- |
| Resource policy | Low-memory/disk, reserved headroom, stale/missing measurements, invalid values, architecture differences, provider overhead, concurrent reservations, changed resources after consent |
| Lifecycle | Replayed/double create, two desktops, multiple OS users, interrupted downloads, crashes at every stage, bootstrap handoff, stop/start, failed resize, reboot, provider-orphan recovery, exactly one active owned guest |
| Security | Pairing/authentication, owner checks on every endpoint, stale/revoked credentials, expired invitations, guest impersonation, path/symlink/argument attacks, no secrets in logs, no unexpected file mounts or management exposure |
| Integration | Real agent/container/proxy/database/volume/registry flows inside the guest; mesh and public/private routing; backup/restore and compatible update rollback |
| UI | Linux bypass; remote physical-host recommendation; accessible keyboard controls and labels; adjust/cancel/accept; resumable progress; second-app reuse; permission/restart blockers; accurate limits and deletion impact |
| Removal | Live/reference rejection, deploy-vs-delete races, shared-image retention, data preservation, offline cleanup, revoke/rejoin behavior, retries and audit results |
| Platform | All nine supported desktop-OS/host-OS pairs; mixed fleets; x86/ARM combinations; native Windows installs and ACLs; Mac boot/login behavior; network changes, reboot, sleep/wake and disk-full recovery |
| Regression | Existing Rust/core/agent/mesh tests, frontend typecheck/unit/browser tests, installer/host scripts, generated types, existing deploy/preview/rollback/database/backup flows, supported screen features |
| Scale | Declared fixture sizes for machines/apps, bounded concurrent jobs, paginated UI/API, bounded event streams, burst reconnect/backoff, database contention and steady-state resource measurements |

Release criteria: all supported platform pairs have real recorded runs; resource promises are measured under idle, build and serving load; first-deployment and later-project flows pass; existing Linux projects/data survive an upgrade; interrupted setup resumes without duplication; unauthorized operations fail; deletion is scoped and recoverable where promised. Test reports distinguish simulated checks, compile checks, actual VM runs and physical hardware checks. Unsupported or unavailable evidence remains a release blocker, not an implied pass.

Benchmark targets for environment idle overhead, minimum viable grant, first setup, boot recovery, serving/build performance and fleet size must be set from measured baselines before general availability. User scale (many independent installations) and per-installation fleet scale are separate; do not introduce a mandatory public central service to solve either prematurely.

## 15. Implementation sequence and remaining engineering decisions

1. Baseline current Linux behavior and write shared contracts, provider fakes and acceptance fixtures.
2. Qualify independent Windows/macOS providers, headless startup, physical-host accounting and network reachability. Resolve provider minimums before presenting recommendations as reliable.
3. Implement native host manager, privileged boundary, native mesh/bootstrap path, durable jobs and singleton ownership.
4. Implement recommendation/admission/enforcement and Linux aggregate-budget integration.
5. Integrate first-deployment UI, later resource settings and existing deployment flow.
6. Complete safe deletion/disconnection/revocation and backward-compatible migration.
7. Complete native installers/updates, mixed-platform end-to-end tests, security/failure testing, load benchmarks and release evidence.

This is an ordering of one complete build, not permission to label an intermediate scaffold complete. Detailed executable steps follow design review.

Engineering calls proposed here: host manager separated from the Linux agent; one host-owned guest; provider adapter boundary; persistent operation journal; owner-only infrastructure changes; conservative measured limits; retain the main-server architecture. Exact provider versions, guest-image packaging, support versions, numeric performance targets and recommendation formula remain implementation qualification decisions, recorded with evidence before release. No sub-agents are used, per the owner's instruction.

## 16. Research references

These sources establish platform constraints, not proof that Vercelify has implemented or passed them:

- [Tauri distribution](https://v2.tauri.app/distribute/): platform-specific installers.
- [WSL configuration](https://learn.microsoft.com/en-us/windows/wsl/wsl-config): global versus per-distribution configuration and shutdown scope.
- [WSL installation](https://learn.microsoft.com/en-us/windows/wsl/install): initial administrative setup and possible restart.
- [Windows virtualization setup](https://support.microsoft.com/en-us/windows/experience/enable-virtualization-on-windows): possible firmware configuration.
- [Lima configuration](https://lima-vm.io/docs/config/): configurable resources and default mounts that must be overridden.
- [Lima automatic startup](https://lima-vm.io/docs/usage/autostart/): boot/login startup and crash recovery.
- [Lima Windows WSL2 driver](https://lima-vm.io/docs/config/vmtype/wsl2/) and [HCS driver](https://lima-vm.io/docs/config/vmtype/hcs/): experimental status and limitations.
- [QEMU Windows Hypervisor Platform](https://www.qemu.org/docs/master/system/whpx.html): independent accelerated-provider candidate.
- [Microsoft Hyper-V installation](https://learn.microsoft.com/en-us/windows-server/virtualization/hyper-v/get-started/install-hyper-v?pivots=windows): edition restrictions; do not equate Hyper-V role support with WHPX qualification.
- [Windows job objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects) and [Docker resource constraints](https://docs.docker.com/engine/containers/resource_constraints/): controls to qualify rather than assume.
