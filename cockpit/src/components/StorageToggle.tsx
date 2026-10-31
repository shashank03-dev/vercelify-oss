import { motion } from "framer-motion";
import { FolderPlus, HardDrive, Zap } from "lucide-react";
import { useMemo, useState } from "react";
import { agent } from "../lib/api";
import { composeSnippet, PRESETS, resolveVolume, VolumeError, type ComposeStyle } from "../lib/compose";
import { bytes, mediaLabel } from "../lib/format";
import type { DiskInfo, StorageTier } from "../types";
import { useToast } from "./Toast";
import { Button, CodeBlock, cx, Field, Segmented, Select, TextInput } from "./ui";

/**
 * Volume creation assistant: pick SSD or HDD and get the exact Docker
 * Compose volume definition (and the bind mount's paths), optionally
 * creating the folder on the host with the right owner.
 */
export function StorageToggle({ disks }: { disks?: DiskInfo[] }) {
  const toast = useToast();
  const [presetId, setPresetId] = useState(PRESETS[0]!.id);
  const preset = PRESETS.find((p) => p.id === presetId) ?? PRESETS[0]!;
  const [service, setService] = useState(preset.id);
  const [name, setName] = useState(preset.name);
  const [containerPath, setContainerPath] = useState(preset.containerPath);
  const [tier, setTier] = useState<StorageTier>("ssd");
  const [style, setStyle] = useState<ComposeStyle>("bind");
  const [uid, setUid] = useState("");
  const [creating, setCreating] = useState(false);

  function applyPreset(id: string) {
    const p = PRESETS.find((x) => x.id === id);
    if (!p) return;
    setPresetId(id);
    setService(p.id);
    setName(p.name);
    setContainerPath(p.containerPath);
    setTier(p.recommended === "hdd" && !disks?.some((d) => d.role === "storage") ? "ssd" : p.recommended);
  }

  const result = useMemo(() => {
    try {
      const resolved = resolveVolume(tier, service, name);
      const snippet = composeSnippet({ service, name, tier, containerPath, style, image: preset.image || undefined });
      return { resolved, snippet, error: null as string | null };
    } catch (e) {
      return { resolved: null, snippet: "", error: e instanceof VolumeError || e instanceof Error ? e.message : String(e) };
    }
  }, [tier, service, name, containerPath, style, preset.image]);

  const ssd = disks?.find((d) => d.role === "system");
  const hdd = disks?.find((d) => d.role === "storage");
  const target = tier === "ssd" ? ssd : hdd;

  async function createOnHost() {
    if (!result.resolved || (tier === "hdd" && !hdd)) return;
    setCreating(true);
    try {
      const owner = uid.trim() ? Number(uid) : null;
      const res = await agent.createVolume({ service, name: name || null, tier, uid: owner, gid: owner });
      toast.ok(res.created ? `Created ${res.host_path}` : `${res.host_path} already exists`);
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setCreating(false);
    }
  }

  const tierCard = (t: StorageTier) => {
    const disk = t === "ssd" ? ssd : hdd;
    const active = tier === t;
    return (
      <button
        key={t}
        type="button"
        onClick={() => setTier(t)}
        data-testid={`tier-${t}`}
        aria-pressed={active}
        disabled={t === "hdd" && !hdd}
        className={cx(
          "relative flex-1 overflow-hidden rounded-lg border p-3.5 text-left transition",
          active ? "border-fg/25 bg-fg/[0.05]" : "border-line bg-surface-2 hover:border-line-strong",
        )}
      >
        {active && <motion.span layoutId="tier-glow" className="absolute inset-x-3 top-0 h-px bg-accent" />}
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-[13px] font-medium">
          {t === "ssd" ? <Zap className="size-4 text-fg-dim" /> : <HardDrive className="size-4 text-fg-dim" />}
          <span className="whitespace-nowrap">{t === "ssd" ? "SSD · fast" : "HDD · big"}</span>
          {preset.recommended === t && <span className="text-[11px] font-normal text-accent-fg">recommended</span>}
        </div>
        <div className="mt-1 text-xs text-fg-muted">
          {t === "ssd" ? "Docker named volume on the system disk" : "Bind mount under /mnt/storage/volumes"}
        </div>
        <div className="mt-2 text-xs text-fg-dim">
          {disk ? `${bytes(disk.available_bytes)} free of ${bytes(disk.total_bytes)}` : t === "hdd" ? "no HDD mounted — single-disk mode" : "—"}
        </div>
      </button>
    );
  };

  return (
    <div className="grid grid-cols-1 gap-6 lg:grid-cols-[1fr_1.1fr]">
      <div className="space-y-4">
        <Field label="What are you storing?">
          <Select value={presetId} onChange={(e) => applyPreset(e.target.value)} data-testid="preset">
            {PRESETS.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label} — {p.why}
              </option>
            ))}
          </Select>
        </Field>
        <div className="flex gap-3">{(["ssd", "hdd"] as const).map(tierCard)}</div>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Service name" hint="compose service / folder">
            <TextInput value={service} onChange={(e) => setService(e.target.value)} data-testid="service-name" spellCheck={false} />
          </Field>
          <Field label="Sub-folder" hint="optional, e.g. data">
            <TextInput value={name} onChange={(e) => setName(e.target.value)} data-testid="volume-name" spellCheck={false} />
          </Field>
          <Field label="Path inside the container">
            <TextInput value={containerPath} onChange={(e) => setContainerPath(e.target.value)} data-testid="container-path" spellCheck={false} />
          </Field>
          <Field label="Owner UID (optional)" hint="e.g. 1000 for most Node images">
            <TextInput value={uid} inputMode="numeric" onChange={(e) => setUid(e.target.value.replace(/\D/g, ""))} />
          </Field>
        </div>
        {tier === "hdd" && (
          <div className="flex items-center justify-between gap-3">
            <span className="text-xs text-fg-muted">Compose style</span>
            <Segmented
              label="compose style"
              value={style}
              onChange={setStyle}
              options={[
                { value: "bind", label: "Bind mount" },
                { value: "named", label: "Named volume → HDD" },
              ]}
            />
          </div>
        )}
      </div>

      <div className="space-y-4">
        {result.error ? (
          <div className="rounded-lg border border-bad/25 bg-bad/[0.07] p-3.5 text-[13px] text-bad" role="alert">
            {result.error}
          </div>
        ) : (
          <>
            <CodeBlock code={result.snippet} testId="compose-snippet" />
            <div className="rounded-lg border border-line bg-surface-2 p-3.5 text-[13px]">
              <div className="mb-2 text-xs font-medium text-fg-muted">As a bind mount (a compose app's volumes, or docker run -v)</div>
              <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 font-mono text-xs">
                <span className="text-fg-dim">Source</span>
                <span className="selectable truncate" data-testid="host-path">
                  {result.resolved?.hostPath}
                </span>
                <span className="text-fg-dim">Destination</span>
                <span className="selectable truncate">{containerPath}</span>
              </div>
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-xs text-fg-dim">
                {target ? `Target disk: ${target.device} (${mediaLabel(target.media) || "unknown media"})` : ""}
              </span>
              <Button icon={<FolderPlus />} busy={creating} onClick={() => void createOnHost()} data-testid="create-volume">
                {tier === "hdd" ? "Create folder on host" : "Create volume on host"}
              </Button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
