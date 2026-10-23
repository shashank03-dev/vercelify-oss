import { ArrowRightLeft, Boxes, CheckCircle2, Circle, HardDrive, RefreshCw, Wand2, XCircle, Zap } from "lucide-react";
import { useState } from "react";
import { StorageToggle } from "../components/StorageToggle";
import { useToast } from "../components/Toast";
import { Button, Callout, Card, cx, EmptyState, ErrorNote, IconButton, Loading, Meter, Modal, OrbitButton, PageHeader, PixelWave, ShimmerText, Skeleton } from "../components/ui";
import { agent, coolify, waitForJob } from "../lib/api";
import { hasCap } from "../lib/caps";
import { useLive } from "../lib/live";
import { bytes, mediaLabel, pct } from "../lib/format";
import { sleep, usePoll } from "../lib/hooks";
import type { CoolifyVolume, DiskInfo, StorageTier } from "../types";

export function StorageView() {
  const sys = usePoll(() => agent.system(), 10_000);
  const vols = usePoll(() => agent.coolifyVolumes(), 8000);
  const raw = usePoll(() => agent.volumes(), 15_000);
  const [moving, setMoving] = useState<{ vol: CoolifyVolume; to: StorageTier } | null>(null);

  const disks = sys.data?.disks ?? [];
  const ssd = disks.find((d) => d.role === "system");
  const hdd = disks.find((d) => d.role === "storage");

  return (
    <>
      <PageHeader
        title="Storage"
        subtitle="Keep the OS, Docker and builds on the fast SSD; put databases and bulk data on the big HDD."
        actions={
          <IconButton label="Refresh" onClick={() => void Promise.all([sys.refresh(), vols.refresh(), raw.refresh()])}>
            <RefreshCw />
          </IconButton>
        }
      />
      <ErrorNote error={sys.error} className="mb-4" />

      <div className="mb-6 grid grid-cols-1 gap-4 md:grid-cols-2">
        <TierCard
          title="SSD tier"
          subtitle="OS · Docker engine · build caches"
          mount="/"
          disk={ssd}
          tone="ssd"
          icon={<Zap className="size-4" />}
          missing={!!sys.data && !ssd}
        />
        <TierCard
          title="HDD tier"
          subtitle="Databases · object storage · large volumes"
          mount="/mnt/storage"
          disk={hdd}
          tone="hdd"
          icon={<HardDrive className="size-4" />}
          missing={!!sys.data && !hdd}
        />
      </div>

      <Card
        title="Coolify volumes"
        description="Persistent data of your apps and databases. Move it between disks with one click."
        icon={<Boxes />}
        className="mb-6"
        testId="coolify-volumes"
        flush
      >
        <ErrorNote error={vols.error} className="m-4" />
        {vols.loading && !vols.data ? (
          <div className="p-4">
            <Loading />
          </div>
        ) : !vols.data?.length ? (
          <EmptyState icon={<Boxes />} title="No persistent volumes yet">
            Databases and apps you create in Coolify show up here, and you can move them between tiers.
          </EmptyState>
        ) : (
          <div role="table" aria-label="Coolify volumes" className="text-[13px]">
            <div role="row" className={cx(VOLUME_GRID, "h-8 border-b border-line bg-fg/[0.015] text-[11px] font-medium text-fg-dim")}>
              <span>Resource</span>
              <span>Volume</span>
              <span>Mounted at</span>
              <span>Disk</span>
              <span />
            </div>
            <div className="divide-y divide-line">
              {vols.data.map((v) => {
                const to: StorageTier = v.tier === "ssd" ? "hdd" : "ssd";
                return (
                  <div role="row" key={v.id} data-testid={`volume-${v.name}`} className={cx(VOLUME_GRID, "py-2.5 transition-colors hover:bg-fg/[0.025]")}>
                    <div className="min-w-0 leading-tight">
                      <div className="truncate font-medium">{v.resource_name || "—"}</div>
                      <div className="mt-1 text-[11px] text-fg-dim">{v.resource_type.replace("Standalone", "")}</div>
                    </div>
                    <span className="truncate font-mono text-xs text-fg-muted" title={v.data_path}>
                      {v.name}
                    </span>
                    <span className="truncate font-mono text-xs text-fg-muted">{v.mount_path}</span>
                    <span className="text-[12px] text-fg-muted">{v.tier === "ssd" ? "SSD" : "HDD"}</span>
                    <span className="flex justify-end">
                      {v.movable ? (
                        <Button
                          size="sm"
                          icon={<ArrowRightLeft />}
                          disabled={to === "hdd" && !hdd}
                          title={to === "hdd" && !hdd ? "No HDD mounted at /mnt/storage" : undefined}
                          onClick={() => setMoving({ vol: v, to })}
                          data-testid={`move-${v.name}`}
                        >
                          Move to {to.toUpperCase()}
                        </Button>
                      ) : (
                        <span className="text-[11px] text-fg-dim">edit compose file</span>
                      )}
                    </span>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </Card>

      <Card
        title="Volume assistant"
        description="Pick a disk and get the exact Compose snippet, or create the folder on the host."
        icon={<Wand2 />}
        className="mb-6"
        testId="storage-toggle"
      >
        <StorageToggle disks={disks} />
      </Card>

      <Card title="On the host" description="Everything Docker and Vercelify have created on each disk" icon={<HardDrive />}>
        <ErrorNote error={raw.error} />
        <div className="grid grid-cols-2 gap-6 text-[13px]">
          {(["hdd", "ssd"] as const).map((t) => {
            const list = (raw.data ?? []).filter((v) => v.tier === t);
            return (
              <div key={t}>
                <div className="mb-2 flex items-baseline gap-2 text-xs">
                  <span className="font-medium text-fg">{t === "hdd" ? "HDD" : "SSD"}</span>
                  <span className="text-fg-dim">
                    {t === "hdd" ? "/mnt/storage/volumes" : "Docker named volumes"} · {list.length}
                  </span>
                </div>
                <ul className="max-h-56 space-y-1 overflow-auto font-mono text-xs text-fg-muted">
                  {list.map((v) => (
                    <li key={v.host_path} className="truncate" title={v.host_path}>
                      {v.name}
                    </li>
                  ))}
                  {!list.length && <li className="text-fg-dim">none</li>}
                </ul>
              </div>
            );
          })}
        </div>
      </Card>

      {moving && (
        <MoveDialog
          vol={moving.vol}
          to={moving.to}
          onClose={() => setMoving(null)}
          onDone={() => {
            void vols.refresh();
            void raw.refresh();
          }}
        />
      )}
    </>
  );
}

const VOLUME_GRID = "grid items-center gap-4 px-4 grid-cols-[minmax(0,1fr)_minmax(0,1fr)_minmax(0,1fr)_56px_128px]";

function TierCard({
  title,
  subtitle,
  mount,
  disk,
  tone,
  icon,
  missing,
}: {
  title: string;
  subtitle: string;
  mount: string;
  disk?: DiskInfo;
  tone: "ssd" | "hdd";
  icon: React.ReactNode;
  missing?: boolean;
}) {
  const used = disk ? pct(disk.used_bytes, disk.total_bytes) : 0;
  return (
    <div className="rounded-xl border border-line bg-surface p-4" data-testid={`tier-card-${tone}`}>
      <div className="mb-4 flex items-start justify-between gap-3">
        <div className="flex items-center gap-3">
          <div className="flex size-9 items-center justify-center rounded-lg bg-fg/[0.06] text-fg-muted">{icon}</div>
          <div>
            <div className="text-[14px] font-semibold tracking-tight">{title}</div>
            <div className="text-xs text-fg-muted">{subtitle}</div>
          </div>
        </div>
        <code className="font-mono text-[11px] text-fg-dim">{mount}</code>
      </div>
      {disk ? (
        <>
          <div className="mb-2 flex items-baseline justify-between">
            <span className="text-[22px] font-semibold tracking-tight tabular-nums">
              {bytes(disk.available_bytes)} <span className="text-[13px] font-normal text-fg-muted">free</span>
            </span>
            <span className="text-xs text-fg-muted tabular-nums">{Math.round(used)}% used</span>
          </div>
          <Meter value={used} label={`${title} usage`} />
          <div className="mt-2.5 flex justify-between text-xs text-fg-dim">
            <span>
              {bytes(disk.used_bytes)} of {bytes(disk.total_bytes)}
            </span>
            <span className="font-mono">
              {disk.device} · {disk.file_system} · {mediaLabel(disk.media) || "unknown media"}
            </span>
          </div>
        </>
      ) : missing && tone === "ssd" ? (
        <Callout>The system disk couldn't be measured (its root filesystem is an overlay or a network mount).</Callout>
      ) : missing ? (
        <Callout>
          No disk mounted at {mount}, so everything lives on the SSD (single-disk mode). Attach a drive and re-run the installer to add
          the HDD tier.
        </Callout>
      ) : (
        <div className="space-y-2.5">
          <Skeleton className="block h-6 w-32" />
          <Skeleton className="block h-1.5 w-full" />
        </div>
      )}
    </div>
  );
}

type StepState = "todo" | "doing" | "done" | "error";

/** Stopped = Coolify says exited AND no container mounts the volume, twice in a row. */
async function waitStopped(vol: CoolifyVolume, isApp: boolean) {
  const deadline = Date.now() + 150_000;
  let streak = 0;
  for (;;) {
    const [volumes, status] = await Promise.all([
      agent.coolifyVolumes(),
      isApp
        ? coolify.applications().then((a) => a.find((x) => x.uuid === vol.resource_uuid)?.status)
        : coolify.databases().then((d) => d.find((x) => x.uuid === vol.resource_uuid)?.status),
    ]);
    const v = volumes.find((x) => x.id === vol.id);
    const stopped = !!v && !v.in_use && !(status ?? "").startsWith("running") && !(status ?? "").startsWith("restarting");
    streak = stopped ? streak + 1 : 0;
    if (streak >= 2) return;
    if (Date.now() > deadline) throw new Error(`${vol.resource_name} did not stop within 2½ minutes`);
    await sleep(2500);
  }
}

/** Stop the resource, copy its data to the other tier, start it again. */
function MoveDialog({ vol, to, onClose, onDone }: { vol: CoolifyVolume; to: StorageTier; onClose: () => void; onDone: () => void }) {
  const toast = useToast();
  const { health } = useLive();
  const [steps, setSteps] = useState<StepState[]>(["todo", "todo", "todo", "todo"]);
  const [error, setError] = useState<string>();
  const [note, setNote] = useState<string>();
  const [running, setRunning] = useState(false);
  const isApp = vol.resource_type === "Application";

  const set = (i: number, s: StepState) => setSteps((prev) => prev.map((v, j) => (j === i ? s : v)));

  async function run() {
    setRunning(true);
    setError(undefined);
    let i = 0;
    try {
      set(0, "doing");
      // Always ask: the volume list may be stale, and a start queued a moment
      // ago (e.g. by a previous move) must not race with the copy.
      try {
        await (isApp ? coolify.appAction(vol.resource_uuid, "stop") : coolify.dbAction(vol.resource_uuid, "stop"));
      } catch (e) {
        if (!/already stopped|not running/i.test((e as Error).message)) throw e;
      }
      set(0, "done");
      i = 1;
      set(1, "doing");
      await waitStopped(vol, isApp);
      set(1, "done");
      i = 2;
      set(2, "doing");
      if (hasCap(health.data, "jobs")) {
        // Large volumes take longer than a request may: follow the job.
        const job = await agent.moveVolumeInBackground(vol.id, to);
        const done = await waitForJob(job.id);
        if (done.status === "failed") throw new Error(done.detail ?? "the move failed");
        setNote(done.detail ?? "");
      } else {
        const res = await agent.moveVolume(vol.id, to);
        setNote(res.note);
      }
      set(2, "done");
      i = 3;
      set(3, "doing");
      await (isApp ? coolify.appAction(vol.resource_uuid, "start") : coolify.dbAction(vol.resource_uuid, "start"));
      set(3, "done");
      toast.ok(`${vol.resource_name} now stores ${vol.mount_path} on the ${to.toUpperCase()}`);
      onDone();
    } catch (e) {
      set(i, "error");
      setError((e as Error).message);
    } finally {
      setRunning(false);
    }
  }

  const labels = [
    `Stop ${vol.resource_name}`,
    "Wait until it has stopped",
    `Copy data to the ${to.toUpperCase()} (owners and permissions preserved)`,
    `Start ${vol.resource_name} on the new location`,
  ];
  const finished = steps.every((s) => s === "done");

  return (
    <Modal open onClose={() => !running && onClose()} title={`Move ${vol.resource_name || vol.name} to ${to.toUpperCase()}`}>
      <p className="mb-4 text-[13px] text-fg-muted">
        {vol.resource_name} will be offline while its data is copied ({to === "hdd" ? "SSD → HDD" : "HDD → SSD"}). The old copy is kept
        until you delete it.
      </p>
      <ol className="mb-4 space-y-2.5" data-testid="move-steps">
        {labels.map((l, i) => (
          <li key={l} className="flex items-center gap-3 text-[13px]">
            {steps[i] === "done" ? (
              <CheckCircle2 className="size-4 text-ok" />
            ) : steps[i] === "doing" ? (
              <span className="flex size-4 items-center justify-center">
                <PixelWave size={3} pixel={3} gap={1.5} />
              </span>
            ) : steps[i] === "error" ? (
              <XCircle className="size-4 text-bad" />
            ) : (
              <Circle className="size-4 text-fg-dim" />
            )}
            {steps[i] === "doing" ? <ShimmerText>{l}</ShimmerText> : <span className={steps[i] === "todo" ? "text-fg-muted" : ""}>{l}</span>}
          </li>
        ))}
      </ol>
      {error && <ErrorNote error={{ message: error }} className="mb-3" />}
      {note && (
        <Callout tone="idle" className="selectable mb-3">
          {note}
        </Callout>
      )}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={onClose} disabled={running}>
          {finished ? "Close" : "Cancel"}
        </Button>
        {!finished && (
          <OrbitButton busy={running} busyLabel="Moving" icon={<ArrowRightLeft />} onRun={run} testId="move-confirm">
            {error ? "Retry" : "Move now"}
          </OrbitButton>
        )}
      </div>
    </Modal>
  );
}
