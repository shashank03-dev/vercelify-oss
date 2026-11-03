import { useState } from "react";
import { hasCap } from "../lib/caps";
import { useLive } from "../lib/live";
import { usePoll } from "../lib/hooks";
import type { PlatformClient, RestoreAssertion, RestoreSchedule } from "../lib/platform";
import type { Database, DatabaseBackup } from "../types";
import { ago } from "../lib/format";
import { Button, Callout, Card, ErrorNote, Field, TextInput, Toggle } from "./ui";

export function RestoreDrills({ db, backups, client }: { db: Database; backups: DatabaseBackup[]; client: PlatformClient }) {
  const { health } = useLive();
  const supported = hasCap(health.data, "postgres-restore-drills") && db.kind === "postgresql" && db.version === "17";
  const jobs = usePoll(() => supported ? client.restoreDrills(db.id) : Promise.resolve([]), supported ? 5000 : 0, [supported, db.id, client]);
  const [backup, setBackup] = useState("");
  const [query, setQuery] = useState("");
  const [expected, setExpected] = useState("");
  const [full, setFull] = useState(false);
  const [savedAssertions, setSavedAssertions] = useState<RestoreAssertion[]>([]);
  const [settings, setSettings] = useState<RestoreSchedule>();
  const [enabled, setEnabled] = useState(false);
  const [cron, setCron] = useState("0 3 1 * *");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<Error>();
  const active = jobs.data?.find(j => !["succeeded", "failed", "cancelled"].includes(j.state));
  const verified = jobs.data?.find(j => j.state === "succeeded" && j.verified_at && !j.cleanup_pending);
  const candidates = backups.filter(b => b.status === "succeeded");
  const selected = backup || candidates[0]?.id || "";
  const assertions: RestoreAssertion[] = [...(query.trim() ? [{ name: savedAssertions[0]?.name ?? "Known result", query, expected }] : []), ...savedAssertions.slice(1)];
  async function act(work: () => Promise<unknown>) {
    setBusy(true); setError(undefined);
    try { await work(); await jobs.refresh(); } catch (e) { setError(e as Error); } finally { setBusy(false); }
  }
  if (db.kind !== "postgresql") return null;
  if (!supported) return <Callout tone="idle">Restore drills need PostgreSQL 17 and an updated server.</Callout>;
  return <Card title="Restore drills" description="Check one backup in a temporary isolated database" testId="restore-drills">
    <div className="space-y-4">
      <p className="text-xs text-fg-muted">Checks recorded relations, indexes, extensions and a read query. Add a known result for the selected backup to check an application invariant.</p>
      <p className="text-sm">{verified ? `Last verified ${ago(verified.verified_at)} · snapshot ${verified.snapshot.slice(0, 12)} · ${verified.restore_seconds ?? "—"} s · backup age at check ${verified.rpo_seconds ?? "—"} s` : "No verified restore yet."}</p>
      {jobs.data?.slice(0, 5).map(job => <div key={job.id} className="border-t border-line pt-3 text-xs" data-testid="restore-drill-result">
        <p>{job.state}{job.cleanup_pending ? " · cleanup pending" : ""}{job.cancel_requested ? " · cancellation requested" : ""} · {job.snapshot.slice(0, 12)} · {job.integrity.replaceAll("_", " ")} · {job.credential_source === "provided" ? "provided recovery key" : "host recovery key"}</p>
        <p>{job.assertion_names.length ? `Known checks: ${job.assertion_names.join(", ")}` : "Catalog and query smoke checks"}</p>
        {job.error && <p className="mt-1 break-words text-fg-muted">{job.error}</p>}
        {!["succeeded", "failed", "cancelled"].includes(job.state) && <Button size="sm" disabled={job.cancel_requested || busy} onClick={() => void act(() => client.cancelRestoreDrill(job.id))}>Cancel drill</Button>}
      </div>)}
      <Field label="Backup to check" hint="Older backups without a manifest require a new backup.">
        <select aria-label="Backup to check" value={selected} onChange={e => setBackup(e.target.value)} className="w-full rounded border border-line bg-bg p-2 text-sm">
          {candidates.map(b => <option key={b.id} value={b.id}>{b.snapshot.slice(0, 12)} · {b.created_at}</option>)}
        </select>
      </Field>
      <Field label="Read query" hint="Optional SELECT or WITH, at most 5 seconds. Use a result known for this backup."><TextInput aria-label="Read query" value={query} onChange={e => setQuery(e.target.value)} maxLength={8192} placeholder="SELECT version FROM migrations" /></Field>
      <Field label="Expected result"><TextInput aria-label="Expected result" value={expected} onChange={e => setExpected(e.target.value)} maxLength={65536} /></Field>
      <Toggle checked={full} onChange={setFull} label="Also read all repository data" hint="Extra time and bandwidth. The selected dump is fully read in either mode." />
      <Button busy={busy} disabled={!selected || !!active} onClick={() => void act(() => client.startRestoreDrill(db.id, selected, assertions, full))}>Run restore drill</Button>
      {!settings ? <Button size="sm" busy={busy} onClick={() => void act(async () => {
        const value = await client.restoreDrillSchedule(db.id); setSettings(value); setEnabled(value.schedule.enabled); setCron(value.schedule.schedule);
        setSavedAssertions(value.schedule.assertions);
        const known = value.schedule.assertions[0]; if (known) { setQuery(known.query); setExpected(known.expected); }
      })}>Configure schedule (owners)</Button> : <div className="space-y-3 border-t border-line pt-3">
        <Toggle checked={enabled} onChange={setEnabled} label="Schedule restore drills" hint="Off by default. Times are UTC; monthly is the default." />
        <Field label="UTC schedule"><TextInput aria-label="UTC schedule" value={cron} onChange={e => setCron(e.target.value)} maxLength={120} /></Field>
        <p className="text-xs text-fg-muted">{settings.overdue ? "Overdue · " : ""}{settings.next_due ? `Next due ${settings.next_due}` : "No next run"}{settings.schedule.error ? ` · ${settings.schedule.error}` : ""}</p>
        <Button busy={busy} onClick={() => void act(async () => { setSettings(await client.setRestoreDrillSchedule(db.id, enabled, cron, assertions)); })}>Save drill schedule</Button>
      </div>}
      <p className="text-xs text-fg-muted">Owners can run and configure drills. Verification covers this snapshot and its selected checks. Host-key checks do not prove recovery after losing the machine.</p>
      <ErrorNote error={error ?? jobs.error} />
    </div>
  </Card>;
}
