import { useState } from "react";
import { Database } from "lucide-react";
import type { App } from "../types";
import type { PlatformClient } from "../lib/platform";
import { hasCap } from "../lib/caps";
import { usePoll } from "../lib/hooks";
import { useLive } from "../lib/live";
import { useToast } from "./Toast";
import { Button, Callout, Card, ErrorNote, Field, StatusText, TextInput, Toggle } from "./ui";

export function PreviewDatabaseCard({ app, client, onChanged }: { app: App; client: PlatformClient; onChanged: () => void }) {
  const { health } = useLive();
  const supported = hasCap(health.data, "preview-databases");
  const status = usePoll(() => supported ? client.previewDatabases(app.id) : Promise.resolve([]), supported ? 20_000 : 0, [supported, app.id, client]);
  if (!health.data) return null;
  if (!supported) return <Callout tone="idle">Update this server to configure isolated PostgreSQL previews.</Callout>;
  return <PreviewDatabaseForm key={`${app.id}:${JSON.stringify(app.previews.database)}`} app={app} client={client} onChanged={onChanged} status={status} />;
}

function PreviewDatabaseForm({ app, client, onChanged, status }: {
  app: App; client: PlatformClient; onChanged: () => void;
  status: ReturnType<typeof usePoll<Awaited<ReturnType<PlatformClient["previewDatabases"]>>>>;
}) {
  const [enabled, setEnabled] = useState(!!app.previews.database);
  const [variable, setVariable] = useState(app.previews.database?.variable ?? "DATABASE_URL");
  const [migration, setMigration] = useState(app.previews.database?.migration ?? "");
  const [seed, setSeed] = useState(app.previews.database?.seed ?? "");
  const [saving, setSaving] = useState(false);
  const toast = useToast();
  async function save() {
    setSaving(true);
    try {
      await client.patchApp(app.id, { previews: { ...app.previews,
        max: enabled ? Math.min(app.previews.max, 3) : app.previews.max,
        database: enabled ? { variable: variable.trim(), migration: migration.trim() || undefined, seed: seed.trim() || undefined } : undefined,
      } });
      toast.ok(enabled ? "Fresh PostgreSQL enabled for previews" : "Preview database cleanup requested");
      onChanged();
      await status.refresh();
    } catch (error) {
      toast.bad((error as Error).message);
    } finally { setSaving(false); }
  }
  return <Card title="Preview PostgreSQL" icon={<Database />} description="Fresh data and restricted credentials for each pull request" testId="preview-database-card">
    <div className="space-y-4">
      <Toggle checked={enabled} onChange={setEnabled} label="Give each PR its own database" testId="preview-database-enabled"
        hint="For trusted PRs. Uses a separate service and new credentials; never copies production data." />
      {enabled && <>
        <Field label="Runtime connection variable" hint="Injected at runtime only. Builds never receive the PR database credential.">
          <TextInput aria-label="Runtime connection variable" value={variable} onChange={(event) => setVariable(event.target.value)} />
        </Field>
        <Field label="Migration command" hint="Optional command already bundled in the app image; runs before publishing.">
          <TextInput aria-label="Migration command" placeholder="npm run migrate" value={migration} onChange={(event) => setMigration(event.target.value)} maxLength={4096} />
        </Field>
        <Field label="Synthetic seed command" hint="Optional test fixtures. Production backups are never copied.">
          <TextInput aria-label="Synthetic seed command" placeholder="npm run seed:preview" value={seed} onChange={(event) => setSeed(event.target.value)} maxLength={4096} />
        </Field>
        <p className="text-xs leading-relaxed text-fg-muted">Main node only, up to 3 active PRs and 4 connections per PR. Keep the app's pool at 3 or fewer so migrations can connect. The shared service defaults to 256 MiB. Data stays until the PR closes; sleeping the app keeps its database. Commands must be bundled in the image because previews have no outbound access.</p>
      </>}
      <ErrorNote error={status.error} />
      {status.data?.map((database) => <div key={database.id} className="flex flex-wrap items-center gap-3 border-t border-line pt-3" data-testid={`preview-database-${database.pr_number}`}>
        <span className="font-mono text-xs">PR #{database.pr_number}</span>
        <StatusText health={database.error ? "bad" : database.state === "ready" ? "ok" : "warn"}
          label={database.error ? "Needs attention" : ({ pending: "Waiting for capacity", provisioned: "Migration pending", ready: "Ready", closing: "Cleanup pending" })[database.state]} />
        {database.error && <p className="w-full break-words text-xs text-fg-muted">{database.error}</p>}
      </div>)}
      <div className="flex justify-end"><Button busy={saving} onClick={() => void save()} disabled={enabled && !variable.trim()} data-testid="save-preview-database">Save database settings</Button></div>
    </div>
  </Card>;
}
