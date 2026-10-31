import { ArrowRightLeft, Boxes, HardDrive, RefreshCw, Wand2, Zap } from "lucide-react";
import { useMemo, useState } from "react";
import { StorageToggle } from "../components/StorageToggle";
import { useToast } from "../components/Toast";
import { Button, Callout, Card, cx, EmptyState, ErrorNote, IconButton, Loading, Meter, Modal, PageHeader, Skeleton, TextInput } from "../components/ui";
import { agent, waitForJob } from "../lib/api";
import { useLiveTopic } from "../lib/live";
import { bytes, mediaLabel, pct } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { isPlatform, platformOn } from "../lib/platform";
import { useSession } from "../lib/session";
import type { DiskInfo, StorageTier, VolumeInfo } from "../types";

export function StorageView() {
  const { settings } = useSession();
  // Workers keep the volumes of the apps placed on them; their main server lists them.
  const own = isPlatform(settings);
  const sys = usePoll(() => agent.system(), 10_000);
  const raw = usePoll(() => agent.volumes(), 15_000);

  const disks = sys.data?.disks ?? [];
  const ssd = disks.find((d) => d.role === "system");
  const hdd = disks.find((d) => d.role === "storage");

  return (
    <>
      <PageHeader
        title="Storage"
        subtitle="Keep the OS, Docker and builds on the fast SSD; put databases and bulk data on the big HDD."
        actions={
          <IconButton label="Refresh" onClick={() => void Promise.all([sys.refresh(), raw.refresh()])}>
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

      {own && <PlatformVolumes hdd={!!hdd} />}

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

/** Volumes of the platform engine's apps and databases, movable between disks. */
function PlatformVolumes({ hdd }: { hdd: boolean }) {
  const { settings } = useSession();
  const client = useMemo(() => platformOn(settings.agent_url), [settings.agent_url]);
  const vols = usePoll(() => client.allVolumes(), 15_000, [client]);
  useLiveTopic("volumes", () => void vols.refresh());
  const toast = useToast();
  const [deleting, setDeleting] = useState<VolumeInfo>();
  const [confirmation, setConfirmation] = useState("");
  const [moving, setMoving] = useState<{ v: VolumeInfo; to: StorageTier } | null>(null);
  const [busy, setBusy] = useState(false);

  async function move() {
    if (!moving) return;
    const { v, to } = moving;
    setBusy(true);
    try {
      const job = await client.moveVolume(v.volume.id, to);
      const done = await waitForJob(job.id, undefined, 2000, settings.agent_url);
      if (done.status === "failed") throw new Error(done.detail ?? "the move failed");
      toast.ok(done.detail ?? `${v.owner_name}/${v.volume.name} is on the ${to.toUpperCase()} now`);
      setMoving(null);
      void vols.refresh();
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Card
        title="Volumes"
        description="Persistent data of your apps and databases. Move it between disks with one click."
        icon={<Boxes />}
        className="mb-6"
        testId="platform-volumes"
        flush
      >
        <ErrorNote error={vols.error} className="m-4" />
        {vols.loading && !vols.data ? (
          <div className="p-4">
            <Loading />
          </div>
        ) : !vols.data?.length ? (
          <EmptyState icon={<Boxes />} title="No persistent volumes yet">
            Databases, and apps with a volume (Settings of the app), show up here.
          </EmptyState>
        ) : (
          <div role="table" aria-label="Volumes" className="text-[13px]">
            <div role="row" className={cx(VOLUME_GRID, "h-8 border-b border-line bg-fg/[0.015] text-[11px] font-medium text-fg-dim")}>
              <span>Resource</span>
              <span>Volume</span>
              <span>Mounted at</span>
              <span>Disk</span>
              <span />
            </div>
            <div className="divide-y divide-line">
              {vols.data.map(({ volume: v, owner_name }) => {
                const to: StorageTier = v.tier === "ssd" ? "hdd" : "ssd";
                return (
                  <div role="row" key={v.id} data-testid={`pvolume-${owner_name}-${v.name}`} className={cx(VOLUME_GRID, "py-2.5 transition-colors hover:bg-fg/[0.025]")}>
                    <div className="min-w-0 leading-tight">
                      <div className="truncate font-medium">{owner_name || "Retained data"}</div>
                      <div className="mt-1 text-[11px] text-fg-dim">{v.owner_kind === "app" ? "App" : "Database"}</div>
                    </div>
                    <span className="truncate font-mono text-xs text-fg-muted" title={v.host_path ?? undefined}>
                      {v.name}
                    </span>
                    <span className="truncate font-mono text-xs text-fg-muted">{v.mount_path}</span>
                    <span className="text-[12px] text-fg-muted">{v.tier === "ssd" ? "SSD" : "HDD"}</span>
                    <span className="flex justify-end">
                      {!owner_name ? <Button size="sm" variant="danger" disabled={v.tier !== "ssd"} onClick={() => { setConfirmation(""); setDeleting({ volume: v, owner_name }); }}>Delete retained data</Button> : <Button
                        size="sm"
                        icon={<ArrowRightLeft />}
                        disabled={to === "hdd" && !hdd}
                        title={to === "hdd" && !hdd ? "No HDD mounted at /mnt/storage" : undefined}
                        onClick={() => setMoving({ v: { volume: v, owner_name }, to })}
                        data-testid={`pmove-${owner_name}-${v.name}`}
                      >
                        Move to {to.toUpperCase()}
                      </Button>}
                    </span>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </Card>
      <Modal open={!!deleting} onClose={() => !busy && setDeleting(undefined)} title="Delete retained data" description="Permanently deletes this volume. This cannot be undone.">
        <p className="mb-2 text-sm">Type {deleting?.volume.name} to delete its data.</p>
        <TextInput value={confirmation} onChange={(e) => setConfirmation(e.target.value)} />
        <Button variant="danger" disabled={confirmation !== deleting?.volume.name} busy={busy} onClick={() => {
          if (!deleting) return;
          setBusy(true);
          void client.deleteRetainedVolume(deleting.volume.id).then(() => { setDeleting(undefined); void vols.refresh(); }, (e: Error) => toast.bad(e.message)).finally(() => setBusy(false));
        }}>Delete permanently</Button>
      </Modal>
      <Modal open={!!moving} onClose={() => !busy && setMoving(null)} title={moving ? `Move ${moving.v.owner_name}/${moving.v.volume.name} to the ${moving.to.toUpperCase()}` : ""}>
        <div className="space-y-3 p-5 text-[13px] text-fg-muted">
          <p>
            {moving?.v.owner_name} stops while its data is copied (owners and permissions are kept), then starts again from the new disk. Large volumes take a while on an HDD.
          </p>
        </div>
        <div className="flex justify-end gap-2 border-t border-line px-5 py-3">
          <Button variant="ghost" disabled={busy} onClick={() => setMoving(null)}>
            Cancel
          </Button>
          <Button variant="primary" busy={busy} onClick={() => void move()} data-testid="pmove-run">
            Move
          </Button>
        </div>
      </Modal>
    </>
  );
}
