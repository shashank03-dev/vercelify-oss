import { ArrowRight, KeyRound, Laptop, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { pairingToSettings, servers as serversApi, settings as settingsApi } from "../lib/api";
import { toCockpitError, type CockpitError } from "../lib/transport";
import type { CockpitSettings, DiscoveredServer } from "../types";
import { Button, cx, ErrorNote, Field, IconButton, InlineCode, PixelWave, ShimmerText, Spinner, StatusDot, TextInput } from "./ui";

/** Swap the host of a URL, keeping scheme and port. */
function withHost(url: string, host: string): string {
  try {
    const u = new URL(url);
    u.hostname = host;
    return u.toString().replace(/\/$/, "");
  } catch {
    return url;
  }
}

/**
 * Connect to a server: pick it from the tailnet and type the 8-character
 * code from `sudo vercelify pair`, or paste the full code.
 */
export function PairForm({
  onPaired,
  paired = [],
  autoScanMs = 0,
  hidePaste = false,
}: {
  onPaired: (s: CockpitSettings, note?: string) => void;
  /** Hostnames already paired (shown as such in the list). */
  paired?: string[];
  /** Keep looking for new machines (while one is being installed). */
  autoScanMs?: number;
  /** Only the tailnet list and address entry (the add-a-machine wizard). */
  hidePaste?: boolean;
}) {
  const [found, setFound] = useState<DiscoveredServer[]>();
  const [scanError, setScanError] = useState<string>();
  const [scanning, setScanning] = useState(false);
  const [picked, setPicked] = useState<DiscoveredServer | null>(null);
  const [short, setShort] = useState("");
  const [address, setAddress] = useState("");
  const [byAddress, setByAddress] = useState(false);
  const [code, setCode] = useState("");
  const [manual, setManual] = useState(false);
  const [form, setForm] = useState<CockpitSettings>({
    agent_url: "http://",
    agent_token: "",
    coolify_url: "http://",
    coolify_token: "",
    hostname: "",
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CockpitError>();

  const scan = useCallback(async () => {
    setScanning(true);
    setScanError(undefined);
    try {
      setFound(await serversApi.discover());
    } catch (e) {
      setFound([]);
      setScanError(toCockpitError(e).message);
    } finally {
      setScanning(false);
    }
  }, []);

  useEffect(() => {
    void scan();
  }, [scan]);

  // While a machine is being installed, rescan until it shows up; open the
  // code field for the first unpaired machine that has a code waiting.
  useEffect(() => {
    if (!autoScanMs) return;
    const id = window.setInterval(() => document.visibilityState === "visible" && void serversApi.discover().then(setFound, () => {}), autoScanMs);
    return () => window.clearInterval(id);
  }, [autoScanMs]);
  const autoPicked = useRef(new Set<string>());
  useEffect(() => {
    if (!autoScanMs || picked || byAddress) return;
    const ready = found?.find((d) => d.pairing_open && !paired.includes(d.hostname) && !autoPicked.current.has(d.agent_url));
    if (ready) {
      autoPicked.current.add(ready.agent_url); // once: Cancel stays cancelled
      setPicked(ready);
    }
  }, [autoScanMs, found, picked, byAddress, paired]);

  /** Test, fall back to the address we found the host at, then save. */
  async function connect(s: CockpitSettings, fallbackHost?: string) {
    setBusy(true);
    setError(undefined);
    try {
      let candidate = s;
      let res;
      try {
        res = await settingsApi.test(candidate);
      } catch (e) {
        // MagicDNS off on this Mac: the IP we discovered still works.
        const host = (() => {
          try {
            return new URL(s.agent_url).hostname;
          } catch {
            return "";
          }
        })();
        if (!fallbackHost || host === fallbackHost) throw e;
        candidate = {
          ...s,
          agent_url: withHost(s.agent_url, fallbackHost),
          coolify_url: s.coolify_url ? withHost(s.coolify_url, fallbackHost) : s.coolify_url,
        };
        res = await settingsApi.test(candidate);
      }
      const final = { ...candidate, hostname: candidate.hostname || res.hostname };
      await settingsApi.save(final);
      const worker = !final.coolify_url;
      onPaired(final, !worker && !res.coolify.ok ? `Connected to the agent, but Coolify said: ${res.coolify.error ?? "unavailable"}` : undefined);
    } catch (e) {
      setError(toCockpitError(e));
    } finally {
      setBusy(false);
    }
  }

  async function claim(target: { agent_url: string; ip?: string }) {
    setBusy(true);
    setError(undefined);
    try {
      const p = await serversApi.claim(target.agent_url, short);
      setBusy(false);
      await connect(pairingToSettings(p), target.ip);
    } catch (e) {
      setError(toCockpitError(e));
      setBusy(false);
    }
  }

  async function pairWithCode() {
    try {
      await connect(pairingToSettings(await settingsApi.decodePairing(code)));
    } catch (e) {
      setError(toCockpitError(e));
    }
  }

  const typedTarget = () => {
    const a = address.trim().replace(/^https?:\/\//, "").replace(/\/.*$/, "");
    if (!a) return null;
    return { agent_url: `http://${a.includes(":") && !a.startsWith("[") ? a : `${a}:7420`}` };
  };

  const shortForm = (target: () => { agent_url: string; ip?: string } | null, hostname: string) => (
    <form
      className="mt-2.5 flex gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        const t = target();
        if (t) void claim(t);
      }}
    >
      <TextInput
        data-testid="short-code"
        placeholder="K7QF-M2XD"
        value={short}
        onChange={(e) => setShort(e.target.value.toUpperCase())}
        autoFocus
        spellCheck={false}
        maxLength={11}
        aria-label={`Pairing code shown on ${hostname}`}
        className="h-9 font-mono tracking-[0.18em]"
      />
      <Button type="submit" variant="primary" busy={busy} disabled={short.replace(/[\s-]/g, "").length !== 8} icon={<ArrowRight />} className="h-9" data-testid="short-submit">
        Connect
      </Button>
    </form>
  );

  return (
    <div className="space-y-5">
      <section data-testid="discovered">
        <div className="mb-2 flex items-center justify-between">
          <div className="text-[13px] font-medium">Servers on your tailnet</div>
          <IconButton label="Scan again" onClick={() => void scan()} disabled={scanning}>
            {scanning ? <Spinner /> : <RefreshCw />}
          </IconButton>
        </div>
        <div className="overflow-hidden rounded-lg border border-line bg-surface">
          {found === undefined ? (
            <div className="flex items-center gap-2 px-3.5 py-3 text-[13px] text-fg-muted">
              <PixelWave size={3} pixel={3} gap={1.5} /> <ShimmerText>Looking for Vercelify servers…</ShimmerText>
            </div>
          ) : found.length === 0 ? (
            <div className="px-3.5 py-3 text-[13px] text-fg-muted">
              {scanError ?? "No servers found yet. Is the installer finished, and is Tailscale signed in on both machines?"}
            </div>
          ) : (
            <ul className="divide-y divide-line">
              {found.map((d) => {
                const already = paired.includes(d.hostname);
                const open = picked?.agent_url === d.agent_url;
                return (
                  <li key={d.agent_url} className="px-3.5 py-2.5" data-testid={`found-${d.hostname}`}>
                    <div className="flex items-center gap-3">
                      <Laptop className="size-4 shrink-0 text-fg-dim" />
                      <div className="min-w-0 flex-1">
                        <div className="flex items-baseline gap-2 text-[13px] font-medium">
                          {d.hostname}
                          <span className="text-[11px] font-normal text-fg-dim">{d.role}</span>
                          {d.pairing_open && !already && (
                            <span className="inline-flex items-center gap-1.5 self-center text-[11px] font-normal text-accent-fg">
                              <StatusDot health="ok" /> code ready
                            </span>
                          )}
                        </div>
                        <div className="truncate font-mono text-[11px] text-fg-dim">{d.dns_name || d.ip}</div>
                      </div>
                      {already ? (
                        <span className="flex items-center gap-1.5 text-xs text-fg-muted">
                          <StatusDot health="ok" /> Paired
                        </span>
                      ) : (
                        <Button size="sm" variant={open ? "ghost" : "subtle"} onClick={() => (setPicked(open ? null : d), setByAddress(false), setShort(""), setError(undefined))}>
                          {open ? "Cancel" : "Connect"}
                        </Button>
                      )}
                    </div>
                    {open && (
                      <div className="mt-2 pl-7">
                        <p className="text-xs text-fg-muted">
                          On {d.hostname} run <InlineCode>sudo vercelify pair</InlineCode> and type the 8-character code it shows.
                        </p>
                        {shortForm(() => d, d.hostname)}
                      </div>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </div>
        {!byAddress ? (
          <button
            type="button"
            className="mt-2 text-xs text-fg-dim underline-offset-2 hover:text-fg-muted hover:underline"
            onClick={() => (setByAddress(true), setPicked(null), setShort(""), setError(undefined))}
          >
            Not listed? Enter its address
          </button>
        ) : (
          <div className="mt-2 rounded-lg border border-line bg-surface p-3">
            <Field label="Tailscale name or IP" hint="e.g. old-thinkpad or 100.101.102.103">
              <TextInput value={address} onChange={(e) => setAddress(e.target.value)} placeholder="old-thinkpad" spellCheck={false} data-testid="pair-address" />
            </Field>
            {shortForm(typedTarget, address || "the server")}
            <button type="button" className="mt-2 text-xs text-fg-dim underline-offset-2 hover:text-fg-muted hover:underline" onClick={() => setByAddress(false)}>
              Cancel
            </button>
          </div>
        )}
      </section>

      {!hidePaste && (
      <section>
        <div className="mb-2 flex items-center gap-2 text-[13px] font-medium">
          <KeyRound className="size-3.5 text-fg-dim" /> Or paste the full pairing code
        </div>
        {!manual ? (
          <form
            className="flex gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              void pairWithCode();
            }}
          >
            <TextInput
              data-testid="pairing-code"
              placeholder="vfy1.eyJhZ2VudF91cmwiOi…"
              value={code}
              onChange={(e) => setCode(e.target.value)}
              spellCheck={false}
              className="h-9 font-mono"
            />
            <Button type="submit" variant="primary" busy={busy} disabled={!code.trim()} icon={<ArrowRight />} className="h-9" data-testid="pair-submit">
              Connect
            </Button>
          </form>
        ) : (
          <form
            className="grid grid-cols-2 gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              void connect(form);
            }}
          >
            <Field label="Agent URL" hint="http://<host>.<tailnet>.ts.net:7420">
              <TextInput value={form.agent_url} onChange={(e) => setForm({ ...form, agent_url: e.target.value })} />
            </Field>
            <Field label="Agent token" hint="/etc/vercelify/agent.token">
              <TextInput type="password" value={form.agent_token} onChange={(e) => setForm({ ...form, agent_token: e.target.value })} />
            </Field>
            <Field label="Coolify URL" hint="http://<host>:8000 (empty for a worker)">
              <TextInput value={form.coolify_url} onChange={(e) => setForm({ ...form, coolify_url: e.target.value })} />
            </Field>
            <Field label="Coolify API token" hint="Coolify → Keys & Tokens → API tokens">
              <TextInput type="password" value={form.coolify_token ?? ""} onChange={(e) => setForm({ ...form, coolify_token: e.target.value })} />
            </Field>
            <div className="col-span-2 flex justify-end">
              <Button type="submit" variant="primary" busy={busy}>
                Connect
              </Button>
            </div>
          </form>
        )}
        <button
          type="button"
          className={cx("mt-2.5 text-xs text-fg-dim underline-offset-2 hover:text-fg-muted hover:underline")}
          onClick={() => setManual(!manual)}
        >
          {manual ? "Use a pairing code instead" : "Enter connection details manually"}
        </button>
      </section>
      )}
      <ErrorNote error={error} />
    </div>
  );
}

export const HOST_INSTALL = "curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh | sudo bash";
export const WORKER_INSTALL = `${HOST_INSTALL} -s -- --worker`;
