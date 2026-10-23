import { Globe, Moon, Plus, X } from "lucide-react";
import { useState } from "react";
import { agent, coolify } from "../lib/api";
import { hasCap } from "../lib/caps";
import { ago } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { useLive, useLiveTopic } from "../lib/live";
import { toCockpitError } from "../lib/transport";
import type { CoolifyApplication, DomainInfo, SleepTarget } from "../types";
import { Toggle } from "./AutoDeploy";
import { NeedsUpdate } from "./Hardware";
import { useToast } from "./Toast";
import { Button, Callout, Field, IconButton, InlineCode, Loading, Select, StatusText, TextInput } from "./ui";

/** `https://a.example.com,http://b.test:3000` → one URL per row. */
export function splitDomains(fqdn: string | null): string[] {
  return (fqdn ?? "")
    .split(",")
    .map((d) => d.trim())
    .filter(Boolean);
}

/** A domain as Coolify wants it, or why not. Bare hosts get https://. */
export function normalizeDomain(raw: string): { url?: string; error?: string } {
  const v = raw.trim().toLowerCase();
  if (!v) return {};
  const url = /^https?:\/\//.test(v) ? v : `https://${v}`;
  try {
    const u = new URL(url);
    if (!/^[a-z0-9.-]+\.[a-z0-9-]+$/.test(u.hostname) || u.username || u.search || u.hash) return { error: `${raw} is not a domain` };
    return { url: `${u.protocol}//${u.host}${u.pathname === "/" ? "" : u.pathname}` };
  } catch {
    return { error: `${raw} is not a domain` };
  }
}

function dnsHint(d: DomainInfo | null | undefined, host: string) {
  if (!d) return null;
  const under = d.domain && (host === d.domain || host.endsWith(`.${d.domain}`));
  if (d.mode === "cloudflare" && under) return "Routed through your Cloudflare tunnel — no DNS changes needed.";
  if (d.mode === "cloudflare") return `Not under ${d.domain}: add the zone to Cloudflare, or point its DNS at this server.`;
  if (d.mode === "direct") return "Point an A record for it at your public IP (ports 80 and 443 forwarded); HTTPS is automatic.";
  return "This server is private (Tailscale only): set a public domain with sudo vercelify domain.";
}

const IDLE_OPTIONS = [15, 30, 60, 120, 240, 720];

function sleepLabel(t: SleepTarget | undefined): { health: "ok" | "idle" | "warn"; label: string } {
  if (!t) return { health: "idle", label: "not reached yet" };
  if (t.state === "asleep") return { health: "idle", label: `asleep since ${ago(t.asleep_since)}` };
  if (t.state === "waking") return { health: "warn", label: "waking up" };
  return { health: "ok", label: t.last_request_at ? `awake · last visit ${ago(t.last_request_at)}` : "awake" };
}

export function AppDomains({ app, onChanged }: { app: CoolifyApplication; onChanged: () => void }) {
  const toast = useToast();
  const { health } = useLive();
  const withDomain = hasCap(health.data, "domain");
  const withSleep = hasCap(health.data, "sleep");
  const domain = usePoll(() => (withDomain ? agent.domain() : Promise.resolve(null)), 0, [withDomain]);
  const sleep = usePoll(() => (withSleep ? agent.sleep() : Promise.resolve(null)), withSleep ? 60_000 : 0, [withSleep]);
  useLiveTopic("sleep", () => void sleep.refresh());

  const [rows, setRows] = useState<string[]>(() => {
    const r = splitDomains(app.fqdn);
    return r.length ? r : [""];
  });
  const [busy, setBusy] = useState<"domains" | "sleep" | null>(null);
  const parsed = rows.map(normalizeDomain);
  const errors = parsed.map((p) => p.error).filter(Boolean);
  const urls = parsed.map((p) => p.url).filter((u): u is string => !!u);
  const changed = urls.join(",") !== splitDomains(app.fqdn).join(",");

  async function saveDomains() {
    setBusy("domains");
    try {
      await coolify.setDomains(app.uuid, urls.join(","));
      toast.ok(urls.length ? `Domains saved — redeploy ${app.name} to apply them` : `${app.name} has no domain now`);
      onChanged();
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setBusy(null);
    }
  }

  const setting = sleep.data?.settings.find((s) => s.app_uuid === app.uuid);
  const target = sleep.data?.targets.find((t) => t.key === app.uuid);

  async function saveSleep(enabled: boolean, idle = setting?.idle_minutes ?? 30, previews = setting?.previews ?? false) {
    setBusy("sleep");
    try {
      await agent.setSleep(app.uuid, { enabled, idle_minutes: idle, previews });
      await sleep.refresh();
      toast.ok(enabled ? `${app.name} sleeps after ${idle} idle minutes` : `${app.name} stays awake`);
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setBusy(null);
    }
  }

  const st = sleepLabel(target);
  return (
    <div className="space-y-6">
      <section className="space-y-3" data-testid="app-domains">
        <h3 className="flex items-center gap-2 text-[13px] font-medium">
          <Globe className="size-4 text-fg-muted" /> Domains
        </h3>
        <div className="space-y-2">
          {rows.map((r, i) => {
            const host = parsed[i]?.url ? new URL(parsed[i]!.url!).hostname : "";
            return (
              <div key={i} className="space-y-1">
                <div className="flex items-center gap-2">
                  <TextInput
                    value={r}
                    placeholder="app.example.com"
                    spellCheck={false}
                    aria-label={`Domain ${i + 1}`}
                    onChange={(e) => setRows(rows.map((x, k) => (k === i ? e.target.value : x)))}
                    data-testid={`domain-${i}`}
                  />
                  <IconButton label="Remove domain" disabled={rows.length === 1 && !r} onClick={() => setRows(rows.length === 1 ? [""] : rows.filter((_, k) => k !== i))}>
                    <X />
                  </IconButton>
                </div>
                {parsed[i]?.error ? (
                  <p className="text-xs text-bad">{parsed[i]!.error}</p>
                ) : (
                  host && <p className="text-xs text-fg-dim">{dnsHint(domain.data, host)}</p>
                )}
              </div>
            );
          })}
        </div>
        <div className="flex items-center gap-2">
          <Button size="sm" variant="ghost" icon={<Plus />} onClick={() => setRows([...rows, ""])}>
            Add a domain
          </Button>
          <span className="flex-1" />
          <Button size="sm" variant="primary" busy={busy === "domains"} disabled={!changed || errors.length > 0} onClick={() => void saveDomains()} data-testid="domains-save">
            Save domains
          </Button>
        </div>
        {domain.data?.wildcard && (
          <p className="text-xs text-fg-dim">
            New apps get <InlineCode>{`<name>.${domain.data.wildcard.replace(/^https?:\/\//, "")}`}</InlineCode> by themselves.
          </p>
        )}
      </section>

      <section className="space-y-3 border-t border-line pt-5" data-testid="app-sleep">
        <h3 className="flex items-center gap-2 text-[13px] font-medium">
          <Moon className="size-4 text-fg-muted" /> Sleep when idle
        </h3>
        {!health.data || (withSleep && sleep.loading) ? (
          <Loading rows={2} />
        ) : !withSleep ? (
          <NeedsUpdate what="Sleeping apps" />
        ) : (
          <>
            <Toggle
              checked={!!setting?.enabled}
              disabled={busy === "sleep"}
              onChange={(v) => void saveSleep(v)}
              label="Stop this app when nobody visits it"
              hint="Gives its memory back to the server. The next visit starts it again; that visitor sees “waking up” for a second or two."
              testId="sleep-toggle"
            />
            {setting?.enabled && (
              <div className="flex flex-wrap items-end gap-4 pl-6">
                <Field label="After">
                  <Select
                    value={String(setting.idle_minutes)}
                    disabled={busy === "sleep"}
                    onChange={(e) => void saveSleep(true, Number(e.target.value))}
                    data-testid="sleep-minutes"
                  >
                    {[...new Set([...IDLE_OPTIONS, setting.idle_minutes])].sort((a, b) => a - b).map((m) => (
                      <option key={m} value={m}>
                        {m < 60 ? `${m} minutes` : `${m / 60} hour${m === 60 ? "" : "s"}`} without visits
                      </option>
                    ))}
                  </Select>
                </Field>
                <Toggle
                  checked={setting.previews}
                  disabled={busy === "sleep"}
                  onChange={(v) => void saveSleep(true, setting.idle_minutes, v)}
                  label="Pull request previews too"
                  hint="They are rarely visited after review."
                />
                <StatusText health={st.health} label={st.label} className="text-xs text-fg-muted" testId="sleep-state" />
              </div>
            )}
            {sleep.data?.error && setting?.enabled && <Callout tone="warn">{sleep.data.error}</Callout>}
          </>
        )}
      </section>
    </div>
  );
}
