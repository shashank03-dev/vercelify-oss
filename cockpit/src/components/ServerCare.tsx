import { Archive, BellRing, Globe, History, KeyRound, Laptop, Send, ShieldAlert, Thermometer, Trash2 } from "lucide-react";
import { useState } from "react";
import { agent, local } from "../lib/api";
import { hasCap } from "../lib/caps";
import { ago, type Health } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { useLive, useLiveTopic } from "../lib/live";
import { notificationsWanted, setNotificationsWanted } from "../lib/notify";
import { useSession } from "../lib/session";
import { toCockpitError } from "../lib/transport";
import type { Alert, PowerGuardSettings } from "../types";
import { NeedsUpdate } from "./Hardware";
import { useToast } from "./Toast";
import { Button, Callout, Card, ErrorNote, Field, IconButton, InlineCode, Loading, LogView, StatusDot, TextInput, Toggle } from "./ui";

export function alertHealth(a: Alert): Health {
  if (a.resolved) return "ok";
  return a.status === "bad" ? "bad" : a.status === "warn" ? "warn" : "idle";
}

/** Where alerts go (ntfy, Discord, Slack, any webhook) and the latest ones. */
export function AlertsCard() {
  const { health, alerts } = useLive();
  const toast = useToast();
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState<"save" | "test" | "check" | null>(null);
  const supported = hasCap(health.data, "alerts");
  const st = alerts.data;
  const [onMac, setOnMac] = useState(notificationsWanted);

  async function save(clear = false) {
    setBusy("save");
    try {
      await agent.setNotifyUrl(clear ? "" : url.trim());
      setUrl("");
      await alerts.refresh();
      toast.ok(clear ? "Notifications off" : "Notifications on — send a test to check");
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setBusy(null);
    }
  }

  async function checkNow() {
    setBusy("check");
    try {
      await agent.checkNow();
      await alerts.refresh();
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setBusy(null);
    }
  }

  async function test() {
    setBusy("test");
    try {
      await agent.testAlert();
      await alerts.refresh();
      toast.ok("Test notification sent");
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setBusy(null);
    }
  }

  return (
    <Card
      title="Alerts"
      icon={<BellRing />}
      description="The server checks itself every minute and tells you when something needs you, even with this app closed"
      testId="alerts"
      actions={
        supported ? (
          <Button size="sm" variant="ghost" busy={busy === "check"} onClick={() => void checkNow()} data-testid="alerts-check">
            Check now
          </Button>
        ) : undefined
      }
    >
      {!health.data ? (
        <Loading rows={2} />
      ) : !supported ? (
        <NeedsUpdate what="Alerts" />
      ) : (
        <div className="space-y-4">
          <div className="flex items-center gap-2 text-[13px]">
            <StatusDot health={st?.notify_target ? "ok" : "warn"} />
            {st?.notify_target ? (
              <>
                <span>
                  Sending to <span className="font-mono text-xs">{st.notify_target}</span>
                </span>
                <span className="flex-1" />
                <Button size="sm" busy={busy === "test"} icon={<Send />} onClick={() => void test()} data-testid="alerts-test">
                  Test
                </Button>
                <Button size="sm" variant="ghost" onClick={() => void save(true)}>
                  Turn off
                </Button>
              </>
            ) : (
              <span className="text-fg-muted">Not sending anywhere yet — alerts only show up here.</span>
            )}
          </div>
          <form
            className="flex items-end gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              void save();
            }}
          >
            <Field label="Notification URL" hint="ntfy topic or Discord/Slack webhook">
              <TextInput value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://ntfy.sh/…" spellCheck={false} data-testid="alerts-url" />
            </Field>
            <Button type="submit" variant="primary" busy={busy === "save"} disabled={!url.trim()}>
              Save
            </Button>
          </form>
          <Toggle
            checked={onMac}
            onChange={(v) => {
              setNotificationsWanted(v);
              setOnMac(v);
            }}
            label="Also notify me on this computer"
            hint="While the app is open: problems and recoveries, as they happen."
            testId="alerts-native"
          />
          <ErrorNote error={alerts.error} />
          {(st?.alerts ?? []).length > 0 && (
            <ul className="divide-y divide-line border-t border-line" data-testid="alerts-list">
              {st!.alerts.slice(0, 8).map((a) => (
                <li key={a.id} className="flex gap-2.5 py-2 text-[13px]">
                  <span className="pt-1.5">
                    <StatusDot health={alertHealth(a)} />
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-medium">{a.title}</span>
                    <span className="block truncate text-xs text-fg-muted">{a.detail}</span>
                  </span>
                  <span className="shrink-0 text-xs text-fg-dim tabular-nums">{ago(a.at)}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </Card>
  );
}

/** Each paired device has its own token; a lost laptop is revoked alone. */
export function DevicesCard() {
  const { health } = useLive();
  const toast = useToast();
  const supported = hasCap(health.data, "tokens");
  const list = usePoll(() => (supported ? agent.tokens() : Promise.resolve([])), 0, [supported]);

  async function revoke(id: string, name: string, current: boolean) {
    if (current && !window.confirm("This is the token this app uses. Revoking it disconnects this app from the server. Continue?")) return;
    try {
      await agent.revokeToken(id);
      toast.ok(`${name} can no longer connect`);
      if (!current) await list.refresh();
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    }
  }

  return (
    <Card title="Paired devices" icon={<Laptop />} description="Revoke one without re-pairing the others" testId="devices" flush>
      {!health.data || (supported && list.loading) ? (
        <div className="p-4">
          <Loading rows={2} />
        </div>
      ) : !supported ? (
        <div className="p-4">
          <NeedsUpdate what="Per-device tokens" />
        </div>
      ) : (list.data ?? []).length === 0 ? (
        <p className="p-4 text-[13px] text-fg-muted">
          This app uses the server's own token (paired before per-device tokens). Pair again with <InlineCode>sudo vercelify pair</InlineCode> to get
          one.
        </p>
      ) : (
        <ul className="divide-y divide-line">
          {(list.data ?? []).map((t) => (
            <li key={t.id} className="flex items-center gap-3 px-4 py-2.5 text-[13px]" data-testid={`device-${t.id}`}>
              <StatusDot health={t.current ? "ok" : "idle"} />
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium">
                  {t.name}
                  {t.current && <span className="ml-2 text-xs font-normal text-fg-dim">this app</span>}
                </span>
                <span className="block text-xs text-fg-dim">
                  added {ago(t.created_at)} · last used {t.last_used_at ? ago(t.last_used_at) : "never"}
                </span>
              </span>
              <IconButton label={`Revoke ${t.name}`} onClick={() => void revoke(t.id, t.name, t.current)}>
                <Trash2 />
              </IconButton>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

/** Nightly encrypted backups: where they go and whether the last one worked. */
export function BackupsCard() {
  const { health } = useLive();
  const { settings } = useSession();
  const toast = useToast();
  const supported = hasCap(health.data, "backup");
  const b = usePoll(() => (supported ? agent.backup() : Promise.resolve(null)), supported ? 15_000 : 0, [supported]);
  useLiveTopic("backup", () => void b.refresh());
  const st = b.data;
  const [saving, setSaving] = useState(false);

  async function run() {
    try {
      await agent.backupRun();
      toast.info(`Backup of ${settings.hostname} started`);
      await b.refresh();
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    }
  }

  async function saveKit() {
    setSaving(true);
    try {
      const kit = await agent.backupRecoveryKit();
      const where = await local.savePrivateFile(kit.filename, kit.text);
      toast.ok(`Recovery kit saved to ${where}. Keep it somewhere other than this server`);
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setSaving(false);
    }
  }

  // Only folders on this machine: one dead disk (or a stolen laptop) loses them too.
  const localOnly = !!st?.targets.length && st.targets.every((t) => t.startsWith("/"));
  const status: Health = !st?.configured ? "warn" : st.running ? "idle" : st.last_ok === false ? "bad" : st.last_ok ? "ok" : "warn";
  return (
    <Card
      title="Backups"
      icon={<Archive />}
      description="The engine's state and secrets, databases and volumes, encrypted"
      testId="backups"
      actions={
        st?.configured ? (
          <Button size="sm" busy={st.running} onClick={() => void run()} data-testid="backup-run">
            {st.running ? "Backing up" : "Back up now"}
          </Button>
        ) : undefined
      }
    >
      {!health.data || (supported && b.loading) ? (
        <Loading rows={2} />
      ) : !supported ? (
        <NeedsUpdate what="Backups" />
      ) : !st?.configured ? (
        <div className="space-y-2 text-[13px]">
          <p className="flex items-center gap-2">
            <StatusDot health="warn" /> Not set up: losing this disk would lose every app's data and secrets.
          </p>
          <p className="text-fg-muted">
            On the server: <InlineCode>sudo vercelify backup setup</InlineCode>. Add another server in your pool as an off-site copy with{" "}
            <InlineCode>--to sftp:root@pi:/mnt/storage/backups/{settings.hostname}</InlineCode>.
          </p>
        </div>
      ) : (
        <div className="space-y-3 text-[13px]">
          <p className="flex items-center gap-2">
            <StatusDot health={status} />
            {st.running
              ? "Backing up now…"
              : st.last_ok == null
                ? "No backup has run yet."
                : st.last_ok
                  ? `Last backup ${ago(st.last_finished_at)}`
                  : `Last backup failed ${ago(st.last_finished_at)}: ${st.last_error ?? ""}`}
          </p>
          {localOnly && (
            <Callout title="On this machine only">
              If this laptop dies or is lost, the backups go with it. Add another server or an S3 bucket:{" "}
              <InlineCode>sudo vercelify backup setup --to sftp:root@pi:/mnt/storage/backups/{settings.hostname}</InlineCode>
            </Callout>
          )}
          <ul className="space-y-1">
            {st.targets.map((t) => (
              <li key={t} className="truncate font-mono text-xs text-fg-muted">
                {t}
              </li>
            ))}
          </ul>
          {st.log_tail.length > 0 && <LogView className="max-h-32">{st.log_tail.join("\n")}</LogView>}
          {hasCap(health.data, "backup-recovery-kit") && (
            <div className="flex items-center justify-between gap-3 border-t border-line pt-3">
              <span className="text-xs text-fg-muted">Without the password nobody can restore these backups. Owners only.</span>
              <Button size="sm" icon={<KeyRound />} busy={saving} onClick={() => void saveKit()} data-testid="backup-recovery-kit">
                Save recovery kit
              </Button>
            </div>
          )}
        </div>
      )}
    </Card>
  );
}

/** Changes made through the agent, by which device. */
export function AuditCard() {
  const { health } = useLive();
  const supported = hasCap(health.data, "audit");
  const a = usePoll(() => (supported ? agent.audit(30) : Promise.resolve([])), 0, [supported]);
  if (!supported) return null;
  return (
    <Card title="Recent changes" icon={<History />} description="Every change made through the agent, and from which device" testId="audit" flush collapsible>
      {a.loading ? (
        <div className="p-4">
          <Loading rows={2} />
        </div>
      ) : (a.data ?? []).length === 0 ? (
        <p className="flex items-center gap-2 p-4 text-[13px] text-fg-muted">
          <ShieldAlert className="size-4" /> Nothing yet.
        </p>
      ) : (
        <ul className="divide-y divide-line">
          {(a.data ?? []).slice(0, 12).map((e, i) => (
            <li key={`${e.at}-${i}`} className="flex items-center gap-3 px-4 py-2 text-xs">
              <StatusDot health={e.status < 400 ? "ok" : "bad"} />
              <span className="w-14 shrink-0 font-mono text-fg-muted">{e.method}</span>
              <span className="min-w-0 flex-1 truncate font-mono">{e.path}</span>
              <span className="w-16 shrink-0 truncate text-fg-dim">{e.actor === "admin" ? "server" : e.actor}</span>
              <span className="w-16 shrink-0 text-right text-fg-dim tabular-nums">{ago(e.at)}</span>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

const PG_FIELDS: { key: keyof PowerGuardSettings; label: string; unit: string; hint: string }[] = [
  { key: "thermal_pause_c", label: "Slow down at", unit: "°C", hint: "Builds and the AI server pause, apps get less CPU" },
  { key: "thermal_freeze_c", label: "Freeze apps at", unit: "°C", hint: "Apps pause too (databases never do)" },
  { key: "thermal_resume_c", label: "Back to normal at", unit: "°C", hint: "Everything resumes" },
  { key: "thermal_shutdown_c", label: "Shut down at", unit: "°C", hint: "After three readings in a row" },
  { key: "thermal_cpu_cap_pct", label: "CPU share while hot", unit: "%", hint: "What each app keeps" },
  { key: "battery_warn_pct", label: "Warn on battery at", unit: "%", hint: "Notification only" },
  { key: "battery_shutdown_pct", label: "Shut down on battery at", unit: "%", hint: "Clean shutdown; apps restart on boot" },
];

/** The power guard's thresholds (it stays its own small service so it works without the agent). */
export function PowerGuardCard() {
  const { health } = useLive();
  const toast = useToast();
  const supported = hasCap(health.data, "power-guard");
  const pg = usePoll(() => (supported ? agent.powerGuard() : Promise.resolve(null)), supported ? 30_000 : 0, [supported]);
  useLiveTopic("power-guard", () => void pg.refresh());
  const [draft, setDraft] = useState<PowerGuardSettings | null>(null);
  const [busy, setBusy] = useState<"save" | "reset" | null>(null);
  const st = pg.data;
  const values = draft ?? st?.settings;

  async function apply(reset: boolean) {
    setBusy(reset ? "reset" : "save");
    try {
      if (reset) await agent.resetPowerGuard();
      else if (draft) await agent.setPowerGuard(draft);
      setDraft(null);
      await pg.refresh();
      toast.ok(reset ? "Power guard back to this machine's defaults" : "Power guard updated");
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setBusy(null);
    }
  }

  const guardHealth: Health = !st ? "idle" : !st.active ? "bad" : st.state?.state === "ok" ? "ok" : st.state?.state ? "warn" : "idle";
  return (
    <Card title="Power guard" icon={<Thermometer />} description="Thermal and battery limits" testId="power-guard" collapsible>
      {!health.data || (supported && pg.loading) ? (
        <Loading rows={3} />
      ) : !supported ? (
        <NeedsUpdate what="Power guard settings" />
      ) : !st || !values ? (
        <ErrorNote error={pg.error} />
      ) : (
        <div className="space-y-4">
          <p className="flex items-center gap-2 text-[13px]">
            <StatusDot health={guardHealth} />
            {st.active ? (st.state?.reason ?? "running") : "not running — the agent restarts it within a minute"}
          </p>
          <div className="grid grid-cols-2 gap-3">
            {PG_FIELDS.map((f) => (
              <Field key={f.key} label={`${f.label} (${f.unit})`} hint={f.hint}>
                <TextInput
                  type="number"
                  inputMode="numeric"
                  value={values[f.key]}
                  onChange={(e) => setDraft({ ...values, [f.key]: Number(e.target.value) })}
                  data-testid={`pg-${f.key}`}
                />
              </Field>
            ))}
          </div>
          <div className="flex items-center gap-2">
            {st.customized && (
              <Button size="sm" variant="ghost" busy={busy === "reset"} onClick={() => void apply(true)}>
                Use this machine's defaults
              </Button>
            )}
            <span className="flex-1" />
            {draft && (
              <Button size="sm" variant="ghost" onClick={() => setDraft(null)}>
                Cancel
              </Button>
            )}
            <Button size="sm" variant="primary" disabled={!draft} busy={busy === "save"} onClick={() => void apply(false)} data-testid="pg-save">
              Save
            </Button>
          </div>
        </div>
      )}
    </Card>
  );
}

const DOMAIN_MODE: Record<string, string> = {
  cloudflare: "Public through a Cloudflare tunnel (works behind CGNAT, no open ports)",
  direct: "Public with your own DNS records (ports 80 and 443 forwarded here)",
  private: "Private: reachable on your Vercelify network only",
};

/** How this server's apps are reached; per-app domains are in each app's Domains tab. */
export function DomainCard() {
  const { health } = useLive();
  const supported = hasCap(health.data, "domain");
  const d = usePoll(() => (supported ? agent.domain() : Promise.resolve(null)), 0, [supported]);
  const info = d.data;
  const tunnel: Health = info?.tunnel_active === false ? "bad" : "ok";
  return (
    <Card title="Domain" icon={<Globe />} description="How visitors reach the apps on this server" testId="domain">
      {!health.data || (supported && d.loading) ? (
        <Loading rows={2} />
      ) : !supported ? (
        <NeedsUpdate what="Domain settings" />
      ) : !info ? (
        <ErrorNote error={d.error} />
      ) : (
        <div className="space-y-3 text-[13px]">
          <p className="flex items-center gap-2">
            <StatusDot health={info.mode === "private" ? "idle" : tunnel} />
            {DOMAIN_MODE[info.mode] ?? info.mode}
            {info.tunnel_active === false && <span className="text-bad">— the tunnel is down</span>}
          </p>
          {info.domain && (
            <p className="text-fg-muted">
              Domain <InlineCode>{info.domain}</InlineCode>
              {info.wildcard && (
                <>
                  ; new apps get <InlineCode>{`<name>.${info.wildcard.replace(/^https?:\/\//, "")}`}</InlineCode>
                </>
              )}
            </p>
          )}
          <p className="text-xs text-fg-dim">
            Change it on the server: <InlineCode>sudo vercelify domain example.com --cloudflare-token …</InlineCode>. Give one app its own domain
            in Apps → the app → Domains.
          </p>
        </div>
      )}
    </Card>
  );
}
