import { useState } from "react";
import { ShieldCheck } from "lucide-react";
import type { App } from "../types";
import type { DeploymentGuard, PlatformClient } from "../lib/platform";
import { hasCap } from "../lib/caps";
import { usePoll } from "../lib/hooks";
import { useLive } from "../lib/live";
import { useToast } from "./Toast";
import { Button, Callout, Card, ErrorNote, Field, StatusText, TextInput, Toggle } from "./ui";

export function DeployGuardCard({ app, client, onChanged }: { app: App; client: PlatformClient; onChanged: () => void }) {
  const { health } = useLive();
  const supported = hasCap(health.data, "deployment-guards");
  const status = usePoll(() => supported ? client.deploymentGuards(app.id) : Promise.resolve([]), supported ? 20_000 : 0, [supported, app.id, client]);
  if (!health.data) return null;
  if (!supported) return <Callout tone="idle" title="Deploy recovery needs a server upgrade">Update this server to configure verified automatic recovery.</Callout>;
  if (app.kind === "compose") return <Callout tone="idle" title="Compose recovery is manual">Use the deployment log and a compatible Compose revision to recover this app.</Callout>;
  return <GuardForm key={`${app.id}:${JSON.stringify(app.runtime)}`} app={app} client={client} onChanged={onChanged} guards={status.data ?? []} error={status.error} />;
}
function GuardForm({ app, client, onChanged, guards, error }: { app: App; client: PlatformClient; onChanged: () => void; guards: DeploymentGuard[]; error?: { message: string } }) {
  const toast = useToast();
  const [enabled, setEnabled] = useState(app.runtime.rollback_compatible ?? false);
  const [grace, setGrace] = useState(String(app.runtime.rollback_grace_secs ?? 10));
  const [failures, setFailures] = useState(String(app.runtime.rollback_failures ?? 3));
  const [saving, setSaving] = useState(false);
  const valid = /^\d+$/.test(grace) && +grace <= 120 && /^\d+$/.test(failures) && +failures >= 1 && +failures <= 10;
  async function save() {
    setSaving(true);
    try {
      await client.patchApp(app.id, { runtime: { ...app.runtime, rollback_compatible: enabled, rollback_grace_secs: +grace, rollback_failures: +failures } });
      toast.ok("Deploy recovery saved"); onChanged();
    } catch (error) { toast.bad(String(error)); }
    finally { setSaving(false); }
  }
  const labels: Record<DeploymentGuard["state"], string> = { watching: "Watching new deployment", requested: "Rollback requested · verification pending", verified: "Previous version verified", failed: "Recovery needs attention", cancelled: "Watch cancelled", expired: "Observation complete" };
  return <Card icon={<ShieldCheck />} title="Deploy recovery" description="Watch a new production version for three minutes. Recovery resumes after an agent restart.">
    <div className="space-y-4" data-testid="deploy-guard-card">
      <Toggle checked={enabled} onChange={setEnabled} label="Allow automatic image rollback" testId="deploy-guard-enabled" hint="Enable only when older images remain compatible with current data and migrations." />
      <p className="text-xs leading-relaxed text-fg-muted">Uses a retained build image or pinned image digest with matching runtime, variables and storage. Older deployments without a compatible snapshot need manual recovery. Image rollback does not undo database migrations or volume writes.</p>
      {enabled && <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Startup grace" hint="Seconds before counting failures, from 0 to 120."><TextInput aria-label="Rollback startup grace" inputMode="numeric" value={grace} onChange={(event) => setGrace(event.target.value)} /></Field>
        <Field label="Consecutive health failures" hint="From 1 to 10. Two added restarts also trigger recovery."><TextInput aria-label="Rollback health failures" inputMode="numeric" value={failures} onChange={(event) => setFailures(event.target.value)} /></Field>
      </div>}
      <ErrorNote error={error} />
      {guards.slice(0, 5).map((guard) => <div key={guard.deployment_id} className="space-y-1 border-t border-line pt-3">
        <StatusText health={guard.state === "verified" ? "ok" : guard.state === "failed" ? "bad" : guard.state === "requested" ? "warn" : "idle"} label={labels[guard.state]} testId="deploy-guard-state" />
        {guard.reason && <p className="text-xs text-fg-muted">{guard.reason}</p>}
      </div>)}
      <div className="flex justify-end"><Button busy={saving} disabled={!valid} onClick={() => void save()} data-testid="save-deploy-guard">Save recovery settings</Button></div>
    </div>
  </Card>;
}
