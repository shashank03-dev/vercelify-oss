import { useState } from "react";
import { MemoryStick } from "lucide-react";
import type { CapacitySample, MemoryReport } from "../types";
import { Card, EmptyState, ErrorNote, Loading, Select, StatusText } from "./ui";

const memory = (value: number | null | undefined) => value == null ? "Unknown" : `${Math.round(value).toLocaleString()} MiB`;

function working(sample: CapacitySample | null, category: string): number | null {
  if (!sample) return null;
  const items = sample.containers.filter((c) => c.running && c.category === category);
  if (items.some((c) => c.usage_mb == null)) return null;
  return items.reduce((sum, c) => sum + c.usage_mb!, 0);
}

function Metric({ label, value, testId }: { label: string; value: string; testId?: string }) {
  return <div className="min-w-0"><dt className="text-[11px] text-fg-muted">{label}</dt><dd data-testid={testId} className="mt-1 font-mono text-sm text-fg">{value}</dd></div>;
}

export function MemoryCard({ reports, error, loading }: { reports?: MemoryReport[]; error?: { message: string } | null; loading?: boolean }) {
  const [selected, setSelected] = useState("");
  const report = reports?.find((r) => r.node_id === selected) ?? reports?.[0];
  return (
    <Card title="Memory budget" icon={<MemoryStick />} description="Observed use, reservations and room for new work" className="mb-6" testId="memory-budget"
      actions={reports && reports.length > 1 ? <Select aria-label="Memory server" value={report?.node_id ?? ""} onChange={(e) => setSelected(e.target.value)}>
        {reports.map((r) => <option key={r.node_id} value={r.node_id}>{r.name}</option>)}
      </Select> : undefined}>
      <ErrorNote error={error} className="mb-4" />
      {loading && !reports ? <Loading rows={3} /> : !report ? (
        <EmptyState icon={<MemoryStick />} title="Capacity unknown">Waiting for a fresh hosting report. Missing measurements are not free capacity.</EmptyState>
      ) : <MemoryDetails report={report} />}
    </Card>
  );
}

function MemoryDetails({ report: r }: { report: MemoryReport }) {
        const s = r.sample;
        const a = r.account;
        const ceiling = s && r.os_reserve_mb != null ? Math.max(0, Math.min(s.workload_limit_mb, s.total_mb - r.os_reserve_mb) - r.safety_mb) : null;
        const headroom = a ? Math.min(a.available_mb, a.workload_remaining_mb) : null;
        const oom = s?.containers.filter((c) => c.oom_killed === true) ?? [];
        const unbounded = s?.containers.some((c) => c.running && ["app", "database"].includes(c.category) && c.limit_mb == null);
        const test = (key: string) => `capacity-${r.node_id}-${key}`;
  return <section className="space-y-4">
          <div className="flex flex-wrap items-start justify-between gap-2">
            <div className="min-w-0 flex-1"><h3 className="break-words text-sm font-medium">{r.name}</h3><p className="mt-1 text-xs text-fg-muted">{r.profile}</p></div>
            <StatusText health={a ? "ok" : "warn"} label={a ? "Fresh capacity" : "New work waits"} />
          </div>
          {r.reason && <p className="text-xs text-fg-muted" role="status">{r.reason}</p>}
          <dl className="grid grid-cols-2 gap-4 sm:grid-cols-3">
            <Metric label="Total RAM" value={memory(s?.total_mb)} />
            <Metric label="System used" value={memory(s ? s.total_mb - s.available_mb : null)} />
            <Metric label="Available · last sample" value={memory(s?.available_mb)} testId={test("available")} />
          </dl>
          <dl className="grid grid-cols-2 gap-4 border-y border-line py-4 sm:grid-cols-4">
            <Metric label="Apps · working memory" value={memory(working(s, "app"))} testId={test("apps")} />
            <Metric label="Databases" value={memory(working(s, "database"))} testId={test("databases")} />
            <Metric label="Builds" value={memory(working(s, "build"))} />
            <Metric label="Platform containers" value={memory(working(s, "platform"))} />
          </dl>
          <p className="text-[11px] text-fg-dim">Container working memory is included in system use. The remaining system use includes the OS, daemons, other processes and cache; it is not measured platform overhead.</p>
          <dl className="grid grid-cols-2 gap-4 sm:grid-cols-3">
            <Metric label="Held for workload growth" value={memory(a?.growth_mb)} />
            <Metric label="Additional reservations" value={memory(a?.reserved_mb)} />
            <Metric label="Committed caps and estimates" value={memory(a?.committed_mb)} />
            <Metric label="Safe admission headroom" value={memory(headroom)} testId={test("headroom")} />
          </dl>
          <p className="text-[11px] leading-relaxed text-fg-muted">OS/platform reserve: {memory(r.os_reserve_mb)} · Safety: {memory(r.safety_mb)} · Admission workload ceiling: {memory(ceiling)}. Swap is not added to capacity.</p>
          {unbounded && <p className="text-xs text-fg-muted">Some existing containers have no memory cap. Their commitments are estimates; current use does not prove their future peak.</p>}
          {s?.host_grant_mb != null && <dl className="grid grid-cols-2 gap-4 sm:grid-cols-3">
            <Metric label="Accepted environment grant" value={memory(s.host_grant_mb)} />
            <Metric label="Physical host available" value={memory(s.host_available_mb)} />
            <Metric label="Provider allowance · outside guest" value={memory(s.provider_overhead_mb)} />
          </dl>}
          <dl className="grid grid-cols-2 gap-4 sm:grid-cols-3">
            <Metric label="Safe temporary disk headroom" value={memory(a?.disk_remaining_mb)} />
            <Metric label="Heavy CPU/I/O slot" value={a ? a.heavy ? "Busy" : "Available" : "Unknown"} />
            <Metric label="Host kernel OOM kills · this boot" value={s?.host_oom_kills == null ? "Unknown" : s.host_oom_kills.toLocaleString()} />
          </dl>
          {oom.map((c) => <p key={c.id} className="text-xs text-fg-muted">Recorded OOM: {c.owner || c.id}{c.finished_at ? ` · ${c.finished_at}` : ""}. Cap {memory(c.limit_mb)}, observed peak {memory(c.peak_mb)}. These readings do not prove the cause of host pressure; exit 137 alone is not OOM evidence.</p>)}
          <p className="text-[10px] text-fg-dim">Policy {r.policy_version} · {r.sample_age_secs == null ? "Sample age unknown" : `Last reported ${Math.max(0, r.sample_age_secs)} seconds old`}. Admission reduces risk; it cannot predict every workload peak.</p>
  </section>;

}
