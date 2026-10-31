import { AlertTriangle, ExternalLink, Laptop, Link2, MonitorPlay, MonitorUp, Play, ScreenShare, Square, TerminalSquare } from "lucide-react";
import { useEffect, useState } from "react";
import { agentOn, local } from "../lib/api";
import { hasCap } from "../lib/caps";
import { useLive } from "../lib/live";
import { usePoll } from "../lib/hooks";
import { agentHost, useSession } from "../lib/session";
import { openExternal } from "../lib/open";
import { inTauri } from "../lib/transport";
import type { CaptureSource, StreamTransport, VideoEncoder } from "../types";
import { useToast } from "./Toast";
import { Button, Callout, Card, cx, Field, InlineCode, OrbitButton, PixelWave, Segmented, Select, ShimmerText, StatusDot, StatusText } from "./ui";

interface Quality {
  id: string;
  label: string;
  width: number | null;
  fps: number;
  kbps: number;
}

export const QUALITIES: Quality[] = [
  { id: "smooth", label: "Smooth — 1080p · 60 fps · 12 Mbps", width: 1920, fps: 60, kbps: 12000 },
  { id: "sharp", label: "Sharp — 1440p · 30 fps · 16 Mbps", width: 2560, fps: 30, kbps: 16000 },
  { id: "native", label: "Native resolution · 60 fps · 20 Mbps", width: null, fps: 60, kbps: 20000 },
  { id: "light", label: "Light — 720p · 30 fps · 4 Mbps (cellular)", width: 1280, fps: 30, kbps: 4000 },
];

/** Servers whose Sunshine this computer's Moonlight is paired with, by agent URL. */
const PAIRED_KEY = "vercelify.moonlight.paired";

function pairedWith(url: string): boolean {
  try {
    return (JSON.parse(localStorage.getItem(PAIRED_KEY) ?? "[]") as string[]).includes(url);
  } catch {
    return false;
  }
}

function rememberPaired(url: string) {
  try {
    const list = (JSON.parse(localStorage.getItem(PAIRED_KEY) ?? "[]") as string[]).filter((u) => u !== url);
    localStorage.setItem(PAIRED_KEY, JSON.stringify([...list, url]));
  } catch {
    // Storage unavailable: the next launch pairs again, which is harmless.
  }
}

/** Four random digits for `moonlight pair --pin`. */
export function randomPin(): string {
  return String(crypto.getRandomValues(new Uint32Array(1))[0]! % 10000).padStart(4, "0");
}

export function DisplayCaster() {
  const isMac = navigator.userAgent.includes("Mac");
  const { settings } = useSession();
  const agent = agentOn(settings.agent_url);
  const toast = useToast();
  const { health } = useLive();
  const canPair = hasCap(health.data, "sunshine-pair");
  const display = usePoll(() => agent.display(), 3000);
  const sender = usePoll(() => local.senderStatus(), 2000);
  const moon = usePoll(() => local.moonlightStatus(), 2000);
  const tools = usePoll(() => local.tools(), 0);
  const screens = usePoll(() => local.captureScreens(), 0);

  const [transport, setTransport] = useState<StreamTransport>("udp");
  const [qualityId, setQualityId] = useState("smooth");
  const [encoder, setEncoder] = useState<VideoEncoder>(inTauri && isMac ? "hardware" : "software");
  const [screen, setScreen] = useState<number | null>(null);
  const [source, setSource] = useState<CaptureSource>(isMac ? "avfoundation" : "test_pattern");
  /** Mirror a screen as it is, or give the laptop's panel a display of its own. */
  const [mode, setMode] = useState<"mirror" | "extend">("mirror");
  const extend = isMac && source === "avfoundation" && mode === "extend";
  const [busy, setBusy] = useState(false);
  const [moonBusy, setMoonBusy] = useState(false);
  const [moonApp, setMoonApp] = useState("Desktop");

  useEffect(() => {
    if (screen === null && screens.data?.length) setScreen(screens.data[screens.data.length - 1]!.index);
  }, [screens.data, screen]);

  const quality = QUALITIES.find((q) => q.id === qualityId) ?? QUALITIES[0]!;
  const tool = (n: string) => tools.data?.find((t) => t.name === n);
  const ffmpeg = tool("ffmpeg");
  const moonlight = tool("moonlight");
  const receiverOn = !!display.data?.receiver.running;
  const senderOn = !!sender.data?.running;
  const streaming = receiverOn && senderOn;
  const live = streaming && display.data?.playback_active === true;
  const captureError = !senderOn && sender.data?.exit_code ? sender.data.last_output.slice(-3).join(" ") : null;

  async function startExtend() {
    setBusy(true);
    let receiverRequested = false;
    try {
      const available = tools.data ?? await local.tools();
      const captureTool = available.find((t) => t.name === "ffmpeg");
      if (!captureTool?.installed) {
        if (!inTauri) throw new Error("Available in the desktop app only — open Vercelify to share a screen.");
        throw new Error(captureTool?.install_hint || "Install ffmpeg on this computer, then refresh setup.");
      }
      const receiver = display.data ?? await agent.display();
      if (!receiver.mpv_installed) throw new Error("Install the Linux display receiver: sudo apt install mpv");
      if (source === "avfoundation" && !(screens.data ?? await local.captureScreens()).length) throw new Error("No screens are available. Allow Screen Recording for Vercelify in System Settings, restart the app, then retry.");
      receiverRequested = true;
      let screenIndex = screen ?? 0;
      if (extend) {
        const width = quality.width ?? 1920;
        screenIndex = (await local.extendDisplayStart(width, Math.round((width * 9) / 16), quality.fps)).index;
      }
      const st = await agent.receiverStart({ transport, port: 5600, latency_ms: transport === "srt" ? 120 : null });
      if (!st.receiver.running) {
        throw new Error(`The display receiver on the host stopped: ${st.receiver.last_output.slice(-3).join(" ") || "no output"}`);
      }
      if (!st.receiver_url) throw new Error("The host has no private address yet: is vercelify-mesh running on it? (sudo vercelify doctor)");
      const started = await local.senderStart({
        url: st.receiver_url,
        source,
        screen_index: screenIndex,
        fps: quality.fps,
        bitrate_kbps: quality.kbps,
        width: quality.width,
        encoder,
        show_cursor: true,
      });
      if (!started.running) throw new Error(started.last_output.slice(-3).join(" ") || "Screen capture stopped before streaming began. Check Screen Recording permission and retry.");
      toast.ok("Stream started — waiting for the Linux receiver to confirm playback");
    } catch (e) {
      toast.bad((e as Error).message);
      if (receiverRequested) {
        await local.senderStop().catch(() => undefined);
        await agent.receiverStop().catch(() => undefined);
      }
    } finally {
      setBusy(false);
      void display.refresh();
      void sender.refresh();
    }
  }

  async function stopExtend() {
    setBusy(true);
    try {
      const stopped = await Promise.allSettled([local.senderStop(), agent.receiverStop()]);
      const failed = stopped.find((result) => result.status === "rejected");
      if (failed?.status === "rejected") throw failed.reason;
      toast.info("Stopped streaming");
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setBusy(false);
      void display.refresh();
      void sender.refresh();
    }
  }

  const [desktopBusy, setDesktopBusy] = useState(false);
  /** `vercelify desktop on` on the server, followed until it ends. */
  async function turnOnDesktop() {
    setDesktopBusy(true);
    try {
      let st = await agent.desktopOn();
      for (let i = 0; st.running && i < 300; i++) {
        await new Promise((r) => setTimeout(r, 2000));
        // It may restart the agent: keep asking.
        st = await agent.desktopOnStatus().catch(() => st);
      }
      if (st.exit_code === 0) toast.ok("Remote desktop is on");
      else toast.bad(st.log_tail.at(-1)?.replace(/^\s*✗\s*/, "") || "Turning on remote desktop failed");
      await display.refresh();
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setDesktopBusy(false);
    }
  }

  async function openSunshine() {
    setMoonBusy(true);
    try { await openExternal(await local.moonlightSetup(agentHost(settings))); }
    catch (e) { toast.bad((e as Error).message); }
    finally { setMoonBusy(false); }
  }

  /**
   * Pairs Moonlight with the server's Sunshine: Moonlight starts pairing with
   * a PIN chosen here and the agent hands Sunshine the same PIN, so nobody
   * makes a Sunshine login or types a PIN.
   */
  async function pairMoonlight() {
    const pin = randomPin();
    await local.moonlightPair(agentHost(settings), pin);
    try {
      const { outcome } = await agent.sunshinePair({ pin, name: "Vercelify" });
      if (outcome === "paired") {
        // Moonlight finishes its side of the exchange, then its window can go.
        for (let i = 0; i < 16 && (await local.moonlightStatus()).running; i++) await new Promise((r) => setTimeout(r, 500));
        toast.ok("Moonlight is paired with this server");
      } else {
        toast.info("Moonlight didn't ask to pair: it's probably paired with this server already");
      }
      rememberPaired(settings.agent_url);
    } finally {
      await local.moonlightStop().catch(() => undefined);
    }
  }

  async function repairMoonlight() {
    setMoonBusy(true);
    try { await pairMoonlight(); }
    catch (e) { toast.bad((e as Error).message); }
    finally { setMoonBusy(false); void moon.refresh(); }
  }

  async function toggleMoonlight() {
    setMoonBusy(true);
    try {
      if (moon.data?.running) await local.moonlightStop();
      else {
        if (canPair && !pairedWith(settings.agent_url)) await pairMoonlight();
        await local.moonlightStart({ host: agentHost(settings), app: moonApp, resolution: null, fps: null, bitrate_kbps: null });
      }
      void moon.refresh();
    } catch (e) {
      toast.bad((e as Error).message);
    } finally { setMoonBusy(false); }
  }

  const sunshine = display.data?.sunshine;
  // Not set up on the server (`vercelify desktop on` never ran there).
  const sunshineOff = sunshine?.active_state === "not-found";

  return (
    <div className="space-y-6">
      <Card
        title="Share a screen"
        description={isMac ? "Mirror a screen, or extend your desktop: the laptop becomes another monitor you can drag windows onto." : "Send a test picture to verify the Linux receiver. Mac screen capture is available in the macOS app."}
        icon={<MonitorUp />}
        testId="extend-card"
        actions={
          receiverOn && !live ? (
            <span className="flex items-center gap-2 text-[12px]">
              <PixelWave size={3} pixel={3} gap={1.5} />
              <ShimmerText>{streaming ? (display.data?.playback_active == null ? "Streaming — update server for playback confirmation" : "Awaiting playback confirmation") : "Waiting for stream"}</ShimmerText>
            </span>
          ) : (
            <StatusText health={live ? "ok" : "idle"} label={live ? "Playing" : "Idle"} testId="extend-status" className="text-[12px] text-fg-muted" />
          )
        }
      >
        {captureError && <div data-testid="capture-error" className="mb-4"><Callout tone="bad" title="Screen capture stopped">{captureError}</Callout></div>}
        <div className="dot-grid relative mb-5 flex items-center gap-4 rounded-xl border border-line bg-bg px-5 py-6">
          <Endpoint icon={<Laptop className="size-5" />} title="This computer" sub="Capture" on={senderOn} />
          <div className="relative h-px flex-1 overflow-hidden bg-line-strong">
            {live && <span className="absolute inset-y-[-1px] left-0 w-1/3 animate-[stream_1.2s_linear_infinite] bg-gradient-to-r from-transparent via-accent to-transparent" />}
          </div>
          <div className="pointer-events-none absolute left-1/2 -translate-x-1/2 -translate-y-5 text-[11px] whitespace-nowrap text-fg-dim">
              {transport.toUpperCase()} over your network · {quality.kbps / 1000} Mbps
          </div>
          <Endpoint icon={<MonitorPlay className="size-5" />} title={settings.hostname} sub="Full screen" on={receiverOn} />
        </div>

        {(tools.error || screens.error || display.error) && <p role="alert" className="mb-4 text-sm text-warn">{display.error?.message || tools.error?.message || screens.error?.message}</p>}
        <Button variant="ghost" size="sm" onClick={() => { void tools.refresh(); void screens.refresh(); void display.refresh(); }} data-testid="refresh-display">Refresh setup</Button>
        <div className="grid grid-cols-2 gap-4">
          <Field label="Screen" hint={extend ? "A new display appears while it streams: arrange it in System Settings → Displays" : undefined}>
            {isMac && source === "avfoundation" && (
              <div className="mb-2">
                <Segmented
                  label="screen mode"
                  value={mode}
                  onChange={setMode}
                  options={[{ value: "mirror", label: "Mirror", testId: "mode-mirror" }, { value: "extend", label: "Extend", testId: "mode-extend" }]}
                />
              </div>
            )}
            {extend ? (
              <Select value="new" disabled>
                <option value="new">New display for {settings.hostname}</option>
              </Select>
            ) : source === "avfoundation" ? (
              <Select value={screen ?? ""} onChange={(e) => setScreen(Number(e.target.value))} disabled={!screens.data?.length} data-testid="screen">
                {!screens.data?.length && <option value="">{inTauri ? "no screens found" : "desktop app only"}</option>}
                {screens.data?.map((s) => (
                  <option key={s.index} value={s.index}>
                    {s.name}
                  </option>
                ))}
              </Select>
            ) : (
              <Select value="pattern" disabled>
                <option value="pattern">Test pattern</option>
              </Select>
            )}
          </Field>
          <Field label="Quality">
            <Select value={qualityId} onChange={(e) => setQualityId(e.target.value)} data-testid="quality">
              {QUALITIES.map((q) => (
                <option key={q.id} value={q.id}>
                  {q.label}
                </option>
              ))}
            </Select>
          </Field>

        </div>

        <details className="mt-4 text-[12px]">
          <summary className="cursor-pointer text-fg-muted" data-testid="display-advanced">Advanced streaming settings</summary>
          <div className="mt-3 grid grid-cols-2 gap-4 lg:grid-cols-3">
          <Field label="Transport" hint={transport === "udp" ? "Lowest latency (home / office)" : "Survives packet loss (cellular)"}>
            <Segmented label="transport" value={transport} onChange={setTransport} options={[{ value: "udp", label: "UDP" }, { value: "srt", label: "SRT" }]} />
          </Field>
          <Field label="Source">
            <Segmented
              label="source"
              value={source}
              onChange={setSource}
              options={[
                ...(isMac ? [{ value: "avfoundation" as const, label: "Screen" }] : []),
                { value: "test_pattern", label: "Test", testId: "source-test" },
              ]}
            />
          </Field>
            <Field label="Encoder">
            <Segmented
              label="encoder"
              value={encoder}
              onChange={setEncoder}
              options={[
                ...(isMac ? [{ value: "hardware" as const, label: "Hardware" }] : []),
                { value: "software", label: "Software" },
              ]}
            />
            </Field>
          </div>
        </details>

        <div className="mt-5 flex flex-wrap items-center justify-between gap-3">
          <div className="flex flex-wrap gap-2 text-[12px]">
            <Prereq ok={!!ffmpeg?.installed} label="ffmpeg on this computer" hint={ffmpeg?.install_hint ?? (inTauri ? "" : "desktop app only")} />
            <Prereq ok={!!display.data?.mpv_installed} label="mpv on the host" hint="sudo apt install mpv" />

          </div>
          {receiverOn || senderOn ? (
            <Button variant="danger" icon={<Square />} busy={busy} onClick={() => void stopExtend()} data-testid="stop-extend">
              Stop
            </Button>
          ) : (
            <OrbitButton icon={<ScreenShare />} busy={busy} busyLabel="Connecting" onRun={startExtend} testId="start-extend">
              Start streaming
            </OrbitButton>
          )}
        </div>

        {(display.data?.receiver.last_output.length || sender.data?.last_output.length) ? (
          <details className="mt-4 text-[12px]">
            <summary className="cursor-pointer text-fg-muted">
              <TerminalSquare className="mr-1 inline size-3.5" />
              Under the hood
            </summary>
            <pre className="mt-2 max-h-48 overflow-auto rounded-lg border border-line bg-bg p-3 font-mono text-[11px] whitespace-pre-wrap text-fg-muted">
              {[
                sender.data?.command && `mac$ ${sender.data.command}`,
                ...(sender.data?.last_output ?? []),
                display.data?.receiver.command && `host$ ${display.data.receiver.command}`,
                ...(display.data?.receiver.last_output ?? []),
              ]
                .filter(Boolean)
                .join("\n")}
            </pre>
          </details>
        ) : null}
      </Card>

      <Card title="Linux desktop on this computer" description="Low-latency remote desktop with Sunshine and Moonlight" icon={<MonitorPlay />} testId="moonlight-card">
        <div className="flex flex-wrap items-center justify-between gap-4">
          <div className="space-y-2 text-[13px]">
            <div className="flex items-center gap-2">
              <StatusDot health={sunshine?.active_state === "active" ? "ok" : sunshine?.active_state === "not-found" ? "idle" : "bad"} />
              Sunshine on the host:{" "}
              <span className="text-fg-muted">{sunshine ? (sunshine.active_state === "not-found" ? "off" : sunshine.active_state) : "…"}</span>
              {sunshineOff && (
                <Button variant="ghost" size="sm" busy={desktopBusy} onClick={() => void turnOnDesktop()} data-testid="desktop-on">Turn on remote desktop</Button>
              )}
            </div>
            <div className="flex items-center gap-2">
              <StatusDot health={moonlight?.installed ? "ok" : "idle"} />
              Moonlight on this computer: <span className="text-fg-muted">{moonlight ? (moonlight.installed ? "installed" : moonlight.install_hint) : "desktop app only"}</span>
            </div>
            {canPair ? (
              <p className="max-w-xl pt-1 text-xs leading-relaxed text-fg-dim" data-testid="moonlight-auto-pair">
                Game-streaming tech: hardware-encoded, about 10 ms on a LAN. The first launch pairs Moonlight with the server by itself: there’s no Sunshine
                login to make and no PIN to type.
              </p>
            ) : (
              <p className="max-w-xl pt-1 text-xs leading-relaxed text-fg-dim">
                Game-streaming tech: hardware-encoded, about 10 ms on a LAN. Launch Moonlight and select the server to see a PIN. Enter that PIN in Sunshine’s web page
                (HTTPS port <InlineCode>47990</InlineCode>; in app network mode, use 127.0.0.1 while Moonlight runs). Pair once, then launch from here. Update the
                server to have this done for you.
              </p>
            )}
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {canPair && (
              <Button variant="ghost" icon={<Link2 />} disabled={!moonlight?.installed} busy={moonBusy} onClick={() => void repairMoonlight()} data-testid="moonlight-pair">Pair again</Button>
            )}
            <Button variant="ghost" icon={<ExternalLink />} busy={moonBusy} onClick={() => void openSunshine()} data-testid="sunshine-setup">Open Sunshine setup</Button>
            <Select value={moonApp} onChange={(e) => setMoonApp(e.target.value)} className="w-36">
              <option>Desktop</option>
              <option>Steam Big Picture</option>
            </Select>
            <Button
              variant={moon.data?.running ? "danger" : "primary"}
              icon={moon.data?.running ? <Square /> : <Play />}
              disabled={!moonlight?.installed || (sunshineOff && !moon.data?.running)}
              busy={moonBusy}
              onClick={() => void toggleMoonlight()}
            >
              {moon.data?.running ? "Close Moonlight" : "Launch Moonlight"}
            </Button>
          </div>
        </div>
      </Card>
    </div>
  );
}

function Endpoint({ icon, title, sub, on }: { icon: React.ReactNode; title: string; sub: string; on: boolean }) {
  return (
    <div className="flex items-center gap-3">
      <div
        className={cx(
          "flex size-10 items-center justify-center rounded-lg border transition",
          on ? "border-fg/25 bg-fg text-inverse" : "border-line-strong bg-surface-2 text-fg-muted",
        )}
      >
        {icon}
      </div>
      <div className="leading-tight">
        <div className="text-[13px] font-medium">{title}</div>
        <div className="text-[11px] text-fg-dim">{sub}</div>
      </div>
    </div>
  );
}

function Prereq({ ok, label, hint }: { ok: boolean; label: string; hint: string }) {
  return (
    <span className={cx("inline-flex h-7 items-center gap-1.5", ok ? "text-fg-muted" : "text-warn")} title={ok ? undefined : hint}>
      {ok ? <StatusDot health="ok" /> : <AlertTriangle className="size-3.5" />}
      {label}
      {!ok && hint && <span className="text-fg-dim">— {hint}</span>}
    </span>
  );
}
