import { Box, Link2, Play, Square, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { pairingToSettings, settings as settingsApi } from "../lib/api";
import { bytes } from "../lib/format";
import { busy as isBusy, fromGb, host, opKey, STAGES, STATE_LABEL, stateHealth, toGb } from "../lib/host";
import { usePoll } from "../lib/hooks";
import { useSession } from "../lib/session";
import { toCockpitError } from "../lib/transport";
import type { EnvironmentGrant, EnvironmentRecommendation, ManagedEnvironment } from "../types";
import { useToast } from "./Toast";
import { Button, Callout, Card, CodeBlock, cx, ErrorNote, Field, Segmented, ShimmerText, StatusText, TextInput } from "./ui";

/**
 * The Vercelify environment on this computer (a Mac or Windows PC): one
 * Linux environment shared by every app it hosts, made by the host manager
 * after the owner agrees to the resources. Hidden where there is no host
 * manager (Linux servers host directly; a browser has none).
 */
export function HostEnvironment() {
  const local = usePoll(() => host.local(), 4000, []);
  const env = local.data?.environment;
  if (!local.data?.installed) return null;
  return (
    <Card
      title="Vercelify environment"
      icon={<Box />}
      description="Where this computer runs your apps: one shared Linux environment, with each app in its own container."
      testId="host-environment"
      actions={env && <StatusText health={stateHealth(env.state)} label={STATE_LABEL[env.state]} testId="host-env-state" />}
    >
      {!local.data.running || !env ? (
        <Callout tone="bad" title="The host manager isn't answering">
          {local.data.error ?? "Start Vercelify's host manager on this computer, then look again."}
        </Callout>
      ) : (
        <Body env={env} refresh={() => void local.refresh()} />
      )}
    </Card>
  );
}

function Body({ env, refresh }: { env: ManagedEnvironment; refresh: () => void }) {
  if (isBusy(env.state)) return <Progress env={env} />;
  if (env.state === "ready" || env.state === "stopped") return <Running env={env} refresh={refresh} />;
  return <Setup env={env} refresh={refresh} />;
}

function Progress({ env }: { env: ManagedEnvironment }) {
  const at = STAGES.findIndex((s) => s.id === env.last_stage);
  return (
    <ol className="space-y-2" data-testid="host-env-progress" aria-live="polite">
      {env.state === "updating" || env.state === "removing" || env.state === "stopping" ? (
        <li className="text-[13px] text-fg">
          <ShimmerText>
            {env.state === "removing" ? "Deleting the environment…" : env.state === "stopping" ? "Stopping the environment…" : "Applying the new resources…"}
          </ShimmerText>
        </li>
      ) : (
        STAGES.map((s, i) => (
          <li key={s.id} className="flex items-center gap-2 text-[13px]">
            <StatusText health={i < at ? "ok" : i === at ? "warn" : "idle"} label={s.label} />
            {i === at && <ShimmerText className="text-xs text-fg-dim">in progress</ShimmerText>}
          </li>
        ))
      )}
    </ol>
  );
}

function GrantFields({
  grant,
  onChange,
  rec,
  minDisk,
}: {
  grant: EnvironmentGrant;
  onChange: (g: EnvironmentGrant) => void;
  rec: EnvironmentRecommendation;
  minDisk?: number;
}) {
  const max = rec.max ?? rec.min;
  const diskMin = Math.max(rec.min.disk_bytes, minDisk ?? 0);
  return (
    <div className="grid gap-3 sm:grid-cols-3">
      <Field label="Memory (GB)" hint={`${toGb(rec.min.memory_bytes)}–${toGb(max.memory_bytes)} GB`}>
        <TextInput
          type="number"
          inputMode="decimal"
          step={0.25}
          min={toGb(rec.min.memory_bytes)}
          max={toGb(max.memory_bytes)}
          value={toGb(grant.memory_bytes)}
          data-testid="host-env-memory"
          aria-label="Memory in GB"
          onChange={(e) => onChange({ ...grant, memory_bytes: fromGb(Number(e.target.value)) })}
        />
      </Field>
      <Field label="Processing power" hint={`${rec.min.cpus}–${max.cpus} processors`}>
        <TextInput
          type="number"
          inputMode="numeric"
          step={1}
          min={rec.min.cpus}
          max={max.cpus}
          value={grant.cpus}
          data-testid="host-env-cpus"
          aria-label="Processors"
          onChange={(e) => onChange({ ...grant, cpus: Math.round(Number(e.target.value)) })}
        />
      </Field>
      <Field label="Storage limit (GB)" hint={`${toGb(diskMin)}–${toGb(max.disk_bytes)} GB`}>
        <TextInput
          type="number"
          inputMode="numeric"
          step={1}
          min={toGb(diskMin)}
          max={toGb(max.disk_bytes)}
          value={toGb(grant.disk_bytes)}
          data-testid="host-env-disk"
          aria-label="Storage limit in GB"
          onChange={(e) => onChange({ ...grant, disk_bytes: fromGb(Math.round(Number(e.target.value))) })}
        />
      </Field>
    </div>
  );
}

function Setup({ env, refresh }: { env: ManagedEnvironment; refresh: () => void }) {
  const toast = useToast();
  const rec = usePoll(() => host.recommendation(), 60_000, []);
  const [grant, setGrant] = useState<EnvironmentGrant>();
  const [mode, setMode] = useState<"join" | "main">("join");
  const [invite, setInvite] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  useEffect(() => {
    if (!grant && rec.data?.grant) setGrant(rec.data.grant);
  }, [rec.data, grant]);

  const r = rec.data;
  const inviteOk = mode === "main" || /^vfyj1\.[A-Za-z0-9._-]+$/.test(invite.trim());
  async function create() {
    if (!r || !grant) return;
    setSaving(true);
    setError(undefined);
    try {
      await host.create({
        idempotency_key: opKey(),
        grant,
        accepted_measured_at: r.measured_at,
        join: mode === "join" ? invite.trim() : undefined,
      });
      toast.ok("Creating the Vercelify environment");
      refresh();
    } catch (e) {
      setError(toCockpitError(e).message);
      void rec.refresh();
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="space-y-4" data-testid="host-env-setup">
      {env.state === "failed" && (
        <Callout tone="bad" title="The last try didn't finish">
          <span className="first-letter:uppercase">{env.reason}</span>
          {env.retry_safe && " Trying again is safe: it picks up from a clean start."}
        </Callout>
      )}
      {env.state === "blocked" && <Callout tone="bad" title="This computer needs a change first">{env.reason}</Callout>}
      <ErrorNote error={rec.error} />
      {r && r.blockers.length > 0 && (
        <Callout tone="bad" title="This computer can't host yet">
          <ul className="list-disc space-y-1 pl-4">
            {r.blockers.map((b) => (
              <li key={b} className="first-letter:uppercase">
                {b}
              </li>
            ))}
          </ul>
        </Callout>
      )}
      {r && grant && r.grant && (
        <>
          <div>
            <p className="mb-3 text-[13px] text-fg-muted">
              <span className="text-fg">Recommended for this computer</span> — you can change it. The environment holds every app, database, build and
              log on this computer; it uses up to this much and leaves the rest to you. The hypervisor itself needs about {bytes(r.provider_overhead_bytes)}{" "}
              more.
            </p>
            <GrantFields grant={grant} onChange={setGrant} rec={r} />
          </div>
          {r.warnings.length > 0 && (
            <Callout tone="warn">
              <ul className="list-disc space-y-1 pl-4">
                {r.warnings.map((w) => (
                  <li key={w} className="first-letter:uppercase">
                    {w}
                  </li>
                ))}
              </ul>
            </Callout>
          )}
          <div className="space-y-2">
            <Segmented
              label="How it joins"
              value={mode}
              onChange={setMode}
              options={[
                { value: "join", label: "Join my servers", testId: "host-env-mode-join" },
                { value: "main", label: "This is my first server", testId: "host-env-mode-main" },
              ]}
            />
            {mode === "join" ? (
              <Field label="Invite from your main server" hint="On the main server: sudo vercelify mesh invite --server">
                <TextInput value={invite} onChange={(e) => setInvite(e.target.value)} placeholder="vfyj1.…" spellCheck={false} data-testid="host-env-invite" />
              </Field>
            ) : (
              <p className="text-xs text-fg-dim">The environment becomes your main server: it keeps your apps' settings and builds them.</p>
            )}
          </div>
          {error && <Callout tone="bad">{error}</Callout>}
          <div className="flex justify-end">
            <Button variant="primary" busy={saving} disabled={!inviteOk} onClick={() => void create()} data-testid="host-env-create">
              {env.state === "failed" ? "Try again" : "Create Vercelify environment"}
            </Button>
          </div>
        </>
      )}
    </div>
  );
}

function Running({ env, refresh }: { env: ManagedEnvironment; refresh: () => void }) {
  const toast = useToast();
  const session = useSession();
  const [code, setCode] = useState<string>();
  const [panel, setPanel] = useState<"none" | "resize" | "delete">("none");
  const [busy, setBusy] = useState<string>();
  const [error, setError] = useState<string>();
  const [confirm, setConfirm] = useState("");
  const [restartOk, setRestartOk] = useState(false);
  const rec = usePoll(() => (panel === "resize" ? host.recommendation() : Promise.resolve(undefined)), 60_000, [panel]);
  const [grant, setGrant] = useState<EnvironmentGrant | undefined>(env.grant ?? undefined);
  const g = env.grant;
  const running = env.state === "ready";

  async function run(key: string, fn: () => Promise<unknown>, ok: string) {
    setBusy(key);
    setError(undefined);
    try {
      await fn();
      toast.ok(ok);
      setPanel("none");
      refresh();
    } catch (e) {
      setError(toCockpitError(e).message);
    } finally {
      setBusy(undefined);
    }
  }

  return (
    <div className="space-y-4" data-testid="host-env-running">
      {g && (
        <dl className="grid grid-cols-3 gap-3 text-[13px]">
          {[
            ["Memory", bytes(g.memory_bytes)],
            ["Processing power", `${g.cpus} processors`],
            ["Storage limit", bytes(g.disk_bytes)],
          ].map(([k, v]) => (
            <div key={k}>
              <dt className="text-xs text-fg-dim">{k}</dt>
              <dd className="text-fg">{v}</dd>
            </div>
          ))}
        </dl>
      )}
      {!running && (
        <Callout tone="idle" title="Stopped">
          Apps on this computer aren't running. They start again with the environment.
        </Callout>
      )}
      <div className="flex flex-wrap gap-2">
        {running ? (
          <Button
            icon={<Square />}
            busy={busy === "stop"}
            data-testid="host-env-stop"
            onClick={() => void run("stop", () => host.stop({ idempotency_key: opKey(), revision: env.revision, acknowledged: true }), "Stopping the environment")}
          >
            Stop
          </Button>
        ) : (
          <Button icon={<Play />} busy={busy === "start"} data-testid="host-env-start" onClick={() => void run("start", () => host.start(), "Starting the environment")}>
            Start
          </Button>
        )}
        {running && env.role === "main" && (
          <>
            <Button
              variant="primary"
              icon={<Link2 />}
              busy={busy === "connect"}
              data-testid="host-env-connect"
              onClick={() =>
                void run(
                  "connect",
                  async () => {
                    const p = await settingsApi.joinPairing((await host.pairing()).code);
                    await session.update(pairingToSettings(p));
                  },
                  "This app now manages the environment",
                )
              }
            >
              Connect this app
            </Button>
            <Button
              busy={busy === "code"}
              data-testid="host-env-code"
              onClick={() =>
                void run("code", async () => setCode((await host.pairing()).code), "A pairing code for another computer (one use, 15 minutes)")
              }
            >
              Pair another computer
            </Button>
          </>
        )}
        <Button variant={panel === "resize" ? "primary" : "subtle"} onClick={() => setPanel(panel === "resize" ? "none" : "resize")} data-testid="host-env-resize">
          Change resources
        </Button>
        <Button variant="danger" icon={<Trash2 />} onClick={() => setPanel(panel === "delete" ? "none" : "delete")} data-testid="host-env-delete">
          Delete environment
        </Button>
      </div>
      {running && (
        <p className="text-xs text-fg-dim">Stopping stops every app on this computer until you start it again.</p>
      )}
      {code && (
        <div className="space-y-1.5" data-testid="host-env-pairing">
          <p className="text-xs text-fg-dim">Paste it into Vercelify on the other computer (one use, 15 minutes). It reaches this one on your network.</p>
          <CodeBlock code={code} />
        </div>
      )}
      {panel === "resize" && rec.data && grant && g && (
        <div className="space-y-3 rounded-lg border border-line p-3">
          <GrantFields grant={grant} onChange={setGrant} rec={rec.data} minDisk={g.disk_bytes} />
          <p className="text-xs text-fg-dim">The storage limit can grow but not shrink.</p>
          {running && (
            <label className="flex items-center gap-2 text-[13px] text-fg">
              <input type="checkbox" checked={restartOk} onChange={(e) => setRestartOk(e.target.checked)} data-testid="host-env-restart-ok" />
              Restart the environment now (its apps stop for a minute or two)
            </label>
          )}
          <div className="flex justify-end">
            <Button
              variant="primary"
              busy={busy === "resize"}
              disabled={running && !restartOk}
              data-testid="host-env-apply"
              onClick={() =>
                void run(
                  "resize",
                  () => host.change({ idempotency_key: opKey(), revision: env.revision, grant, restart_ok: restartOk }),
                  "Applying the new resources",
                )
              }
            >
              Apply
            </Button>
          </div>
        </div>
      )}
      {panel === "delete" && (
        <div className={cx("space-y-3 rounded-lg border border-bad/30 p-3")}>
          <p className="text-[13px] text-fg">
            This deletes the environment and <span className="font-medium">every app, database, volume and backup stored in it</span>. Move or back up what
            you need first. Apps on your other servers aren't affected.
          </p>
          <Field label="Type delete to confirm">
            <TextInput value={confirm} onChange={(e) => setConfirm(e.target.value)} spellCheck={false} data-testid="host-env-delete-confirm" />
          </Field>
          <div className="flex justify-end">
            <Button
              variant="danger"
              icon={<Trash2 />}
              disabled={confirm !== "delete"}
              busy={busy === "delete"}
              data-testid="host-env-delete-yes"
              onClick={() => void run("delete", () => host.remove({ idempotency_key: opKey(), revision: env.revision, acknowledged: true }), "Deleting the environment")}
            >
              Delete everything in it
            </Button>
          </div>
        </div>
      )}
      {error && <Callout tone="bad">{error}</Callout>}
    </div>
  );
}
