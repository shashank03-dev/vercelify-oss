import { ArrowRight, KeyRound, Laptop, RefreshCw, Wifi } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { mesh, pairingToSettings, servers as serversApi, settings as settingsApi } from "../lib/api";
import { toCockpitError, type CockpitError } from "../lib/transport";
import type { CockpitSettings, DiscoveredServer, LanServer } from "../types";
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
 * Connect to a server: pick it from the LAN or the mesh and type the 8-character
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
  /** Only the server lists and address entry (the add-a-machine wizard). */
  hidePaste?: boolean;
}) {
  const [found, setFound] = useState<DiscoveredServer[]>();
  /** Servers on this LAN (mDNS, Vercelify's own network). */
  const [lan, setLan] = useState<LanServer[]>([]);
  const [lanPicked, setLanPicked] = useState<string | null>(null);
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
    hostname: "",
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CockpitError>();

  const scan = useCallback(async () => {
    setScanning(true);
    setScanError(undefined);
    void mesh.discover().then(setLan, () => setLan([]));
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

  // Installed with `install-mac.sh --pair CODE`: pair with it right away.
  useEffect(() => {
    if (hidePaste) return;
    void settingsApi.pendingPairing().then(
      (c) => {
        if (!c) return;
        setCode(c);
        void pairWithCode(c);
      },
      () => {},
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
        // The name doesn't resolve from this Mac: the IP we found still works.
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
        };
        res = await settingsApi.test(candidate);
      }
      const final = { ...candidate, hostname: candidate.hostname || res.hostname };
      await settingsApi.save(final);
      onPaired(final);
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

  /** A server found on the LAN: the code is checked with SPAKE2, then this computer joins the mesh. */
  async function claimLan(s: LanServer) {
    setBusy(true);
    setError(undefined);
    try {
      const p = await mesh.pair(short, s.control[0]);
      setBusy(false);
      await connect(pairingToSettings(p));
    } catch (e) {
      setError(toCockpitError(e));
      setBusy(false);
    }
  }

  async function pairWithCode(given = code) {
    try {
      // A vfy2. code also puts this computer on the server's mesh.
      setBusy(true);
      const p = await settingsApi.joinPairing(given);
      setBusy(false);
      await connect(pairingToSettings(p));
    } catch (e) {
      setError(toCockpitError(e));
      setBusy(false);
    }
  }

  const typedTarget = () => {
    const a = address.trim().replace(/^https?:\/\//, "").replace(/\/.*$/, "");
    if (!a) return null;
    return { agent_url: `http://${a.includes(":") && !a.startsWith("[") ? a : `${a}:7420`}` };
  };

  const shortForm = (target: () => { agent_url: string; ip?: string } | null, hostname: string, onCode?: () => void) => (
    <form
      className="mt-2.5 flex gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        if (onCode) return void onCode();
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
      {lan.length > 0 && (
        <section data-testid="lan-servers">
          <div className="mb-2 flex items-center gap-2 text-[13px] font-medium">
            <Wifi className="size-3.5 text-fg-dim" /> Servers on this network
          </div>
          <ul className="divide-y divide-line overflow-hidden rounded-lg border border-line bg-surface">
            {lan.map((d) => {
              const already = paired.includes(d.name);
              const open = lanPicked === d.name;
              return (
                <li key={d.name} className="px-3.5 py-2.5" data-testid={`lan-${d.name}`}>
                  <div className="flex items-center gap-3">
                    <Laptop className="size-4 shrink-0 text-fg-dim" />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-baseline gap-2 text-[13px] font-medium">
                        {d.name}
                        {d.pairing_open && !already && (
                          <span className="inline-flex items-center gap-1.5 self-center text-[11px] font-normal text-accent-fg">
                            <StatusDot health="ok" /> code ready
                          </span>
                        )}
                      </div>
                      <div className="truncate font-mono text-[11px] text-fg-dim">{d.control[0]} · Vercelify network</div>
                    </div>
                    {already ? (
                      <span className="flex items-center gap-1.5 text-xs text-fg-muted">
                        <StatusDot health="ok" /> Paired
                      </span>
                    ) : (
                      <Button size="sm" variant={open ? "ghost" : "subtle"} onClick={() => (setLanPicked(open ? null : d.name), setPicked(null), setByAddress(false), setShort(""), setError(undefined))}>
                        {open ? "Cancel" : "Connect"}
                      </Button>
                    )}
                  </div>
                  {open && (
                    <div className="mt-2 pl-7">
                      <p className="text-xs text-fg-muted">
                        On {d.name} run <InlineCode>sudo vercelify pair</InlineCode> and type the 8-character code it shows. This computer joins its
                        network.
                      </p>
                      {shortForm(() => null, d.name, () => void claimLan(d))}
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        </section>
      )}
      <section data-testid="discovered">
        <div className="mb-2 flex items-center justify-between">
          <div className="text-[13px] font-medium">Servers on your Vercelify network</div>
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
              {scanError ?? "No servers found yet. Is the installer finished? A server on this Wi-Fi shows up above; one elsewhere, by its address below."}
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
                        <div className="truncate font-mono text-[11px] text-fg-dim">{d.ip} · Vercelify network</div>
                      </div>
                      {already ? (
                        <span className="flex items-center gap-1.5 text-xs text-fg-muted">
                          <StatusDot health="ok" /> Paired
                        </span>
                      ) : (
                        <Button size="sm" variant={open ? "ghost" : "subtle"} onClick={() => (setPicked(open ? null : d), setLanPicked(null), setByAddress(false), setShort(""), setError(undefined))}>
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
            <Field label="Name or address" hint="e.g. old-thinkpad.local, 192.168.1.20 or 10.77.0.1">
              <TextInput value={address} onChange={(e) => setAddress(e.target.value)} placeholder="old-thinkpad.local" spellCheck={false} data-testid="pair-address" />
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
              placeholder="vfy2.eyJhZ2VudF91cmwiOi…"
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
            <Field label="Agent URL" hint="http://10.77.0.1:7420 (the server's mesh or LAN address)">
              <TextInput value={form.agent_url} onChange={(e) => setForm({ ...form, agent_url: e.target.value })} />
            </Field>
            <Field label="Agent token" hint="/etc/vercelify/agent.token">
              <TextInput type="password" value={form.agent_token} onChange={(e) => setForm({ ...form, agent_token: e.target.value })} />
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
