import { AlertTriangle, Laptop, MonitorPlay, MonitorUp, Play, ScreenShare, Square, TerminalSquare } from "lucide-react";
import { useEffect, useState } from "react";
import { agentOn, local } from "../lib/api";
import { usePoll } from "../lib/hooks";
import { agentHost, useSession } from "../lib/session";
import { inTauri } from "../lib/transport";
import type { CaptureSource, StreamTransport, VideoEncoder } from "../types";
import { useToast } from "./Toast";
import { Button, Card, cx, Field, InlineCode, OrbitButton, PixelWave, Segmented, Select, ShimmerText, StatusDot, StatusText } from "./ui";

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

export function DisplayCaster() {
  const { settings } = useSession();
  const agent = agentOn(settings.agent_url);
  const toast = useToast();
  const display = usePoll(() => agent.display(), 3000);
  const sender = usePoll(() => local.senderStatus(), 2000);
  const moon = usePoll(() => local.moonlightStatus(), 2000);
  const tools = usePoll(() => local.tools(), 0);
  const screens = usePoll(() => local.captureScreens(), 0);

  const [transport, setTransport] = useState<StreamTransport>("udp");
  const [qualityId, setQualityId] = useState("smooth");
  const [encoder, setEncoder] = useState<VideoEncoder>(inTauri && navigator.userAgent.includes("Mac") ? "hardware" : "software");
  const [screen, setScreen] = useState<number | null>(null);
  const [source, setSource] = useState<CaptureSource>(navigator.userAgent.includes("Mac") ? "avfoundation" : "test_pattern");
  const [busy, setBusy] = useState(false);
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
  const live = receiverOn && senderOn;

  async function startExtend() {
    setBusy(true);
    try {
      const st = await agent.receiverStart({ transport, port: 5600, latency_ms: transport === "srt" ? 120 : null });
      if (!st.receiver.running) {
        throw new Error(`The display receiver on the host stopped: ${st.receiver.last_output.slice(-3).join(" ") || "no output"}`);
      }
      if (!st.receiver_url) throw new Error("The host has no private address yet: is vercelify-mesh running on it? (sudo vercelify doctor)");
      await local.senderStart({
        url: st.receiver_url,
        source,
        screen_index: screen ?? 0,
        fps: quality.fps,
        bitrate_kbps: quality.kbps,
        width: quality.width,
        encoder,
        show_cursor: true,
      });
      toast.ok("Streaming to the Linux screen");
      void display.refresh();
      void sender.refresh();
    } catch (e) {
      toast.bad((e as Error).message);
      await agent.receiverStop().catch(() => undefined);
    } finally {
      setBusy(false);
    }
  }

  async function stopExtend() {
    setBusy(true);
    try {
      await local.senderStop().catch(() => undefined);
      await agent.receiverStop();
      toast.info("Stopped streaming");
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setBusy(false);
      void display.refresh();
      void sender.refresh();
    }
  }

  async function toggleMoonlight() {
    try {
      if (moon.data?.running) await local.moonlightStop();
      else await local.moonlightStart({ host: agentHost(settings), app: moonApp, resolution: null, fps: null, bitrate_kbps: null });
      void moon.refresh();
    } catch (e) {
      toast.bad((e as Error).message);
    }
  }

  const sunshine = display.data?.sunshine;

  return (
    <div className="space-y-6">
      <Card
        title="Share your Mac screen"
        description="Mirror an existing screen. To extend your desktop, first create a virtual display or connect a display adapter."
        icon={<MonitorUp />}
        testId="extend-card"
        actions={
          receiverOn && !live ? (
            <span className="flex items-center gap-2 text-[12px]">
              <PixelWave size={3} pixel={3} gap={1.5} />
              <ShimmerText>Waiting for stream</ShimmerText>
            </span>
          ) : (
            <StatusText health={live ? "ok" : "idle"} label={live ? "Live" : "Idle"} testId="extend-status" className="text-[12px] text-fg-muted" />
          )
        }
      >
        <div className="dot-grid relative mb-5 flex items-center gap-4 rounded-xl border border-line bg-bg px-5 py-6">
          <Endpoint icon={<Laptop className="size-5" />} title="This Mac" sub={`ffmpeg ${encoder === "hardware" ? "VideoToolbox" : "x264"}`} on={senderOn} />
          <div className="relative h-px flex-1 overflow-hidden bg-line-strong">
            {live && <span className="absolute inset-y-[-1px] left-0 w-1/3 animate-[stream_1.2s_linear_infinite] bg-gradient-to-r from-transparent via-accent to-transparent" />}
          </div>
          <div className="pointer-events-none absolute left-1/2 -translate-x-1/2 -translate-y-5 text-[11px] whitespace-nowrap text-fg-dim">
              {transport.toUpperCase()} over your network · {quality.kbps / 1000} Mbps
          </div>
          <Endpoint icon={<MonitorPlay className="size-5" />} title={settings.hostname} sub="mpv · fullscreen" on={receiverOn} />
        </div>

        <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
          <Field label="Mac screen" hint={screens.data?.length ? "Tip: a virtual display (e.g. BetterDisplay) makes it a true extended desktop" : undefined}>
            {source === "avfoundation" ? (
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
          <Field label="Transport" hint={transport === "udp" ? "Lowest latency (home / office)" : "Survives packet loss (cellular)"}>
            <Segmented label="transport" value={transport} onChange={setTransport} options={[{ value: "udp", label: "UDP" }, { value: "srt", label: "SRT" }]} />
          </Field>
          <Field label="Source">
            <Segmented
              label="source"
              value={source}
              onChange={setSource}
              options={[
                { value: "avfoundation", label: "Screen" },
                { value: "test_pattern", label: "Test", testId: "source-test" },
              ]}
            />
          </Field>
        </div>

        <div className="mt-5 flex flex-wrap items-center justify-between gap-3">
          <div className="flex flex-wrap gap-2 text-[12px]">
            <Prereq ok={!!ffmpeg?.installed} label="ffmpeg on this Mac" hint={ffmpeg?.install_hint ?? (inTauri ? "" : "desktop app only")} />
            <Prereq ok={!!display.data?.mpv_installed} label="mpv on the host" hint="sudo apt install mpv" />
            <Segmented
              label="encoder"
              value={encoder}
              onChange={setEncoder}
              options={[
                { value: "hardware", label: "Hardware" },
                { value: "software", label: "Software" },
              ]}
            />
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

      <Card title="Linux desktop on this Mac" description="Low-latency remote desktop with Sunshine and Moonlight" icon={<MonitorPlay />} testId="moonlight-card">
        <div className="flex flex-wrap items-center justify-between gap-4">
          <div className="space-y-2 text-[13px]">
            <div className="flex items-center gap-2">
              <StatusDot health={sunshine?.active_state === "active" ? "ok" : sunshine?.active_state === "not-found" ? "idle" : "bad"} />
              Sunshine on the host:{" "}
              <span className="text-fg-muted">{sunshine ? (sunshine.active_state === "not-found" ? "not installed" : sunshine.active_state) : "…"}</span>
            </div>
            <div className="flex items-center gap-2">
              <StatusDot health={moonlight?.installed ? "ok" : "idle"} />
              Moonlight on this Mac: <span className="text-fg-muted">{moonlight ? (moonlight.installed ? "installed" : moonlight.install_hint) : "desktop app only"}</span>
            </div>
            <p className="max-w-xl pt-1 text-xs leading-relaxed text-fg-dim">
              Game-streaming tech: hardware-encoded, about 10 ms on a LAN. Launch Moonlight and select the server to see a PIN. Enter that PIN in Sunshine’s web page
              (HTTPS port <InlineCode>47990</InlineCode>; in app network mode, use 127.0.0.1 while Moonlight runs). Pair once, then launch from here.
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Select value={moonApp} onChange={(e) => setMoonApp(e.target.value)} className="w-36">
              <option>Desktop</option>
              <option>Steam Big Picture</option>
            </Select>
            <Button
              variant={moon.data?.running ? "danger" : "primary"}
              icon={moon.data?.running ? <Square /> : <Play />}
              disabled={!moonlight?.installed}
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
