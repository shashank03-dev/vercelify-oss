import { render, screen, waitFor } from "@testing-library/react";
import { SessionProvider } from "../lib/session";
import { ToastProvider } from "./Toast";
import { DisplayCaster } from "./DisplayCaster";
import type { CockpitSettings } from "../types";

vi.mock("../lib/live", () => ({ useLive: () => ({ health: { data: { capabilities: [] } } }) }));

const sent = vi.hoisted(() => [] as { cmd: string; args?: Record<string, unknown> }[]);
const stopped = vi.hoisted(() => ({ capture: true }));
const meshPath = vi.hoisted(() => ({ endpoint: "", noSrt: false }));

vi.mock("../lib/transport", async (original) => ({
  ...await original<typeof import("../lib/transport")>(),
  call: async (cmd: string, args?: Record<string, unknown>) => {
    sent.push({ cmd, args });
    if (cmd === "extend_display_start") return { index: 4, name: "Capture screen 1" };
    if (cmd === "tools_status" && meshPath.endpoint)
      return [{ name: "ffmpeg", installed: true, path: "/opt/homebrew/bin/ffmpeg", install_hint: "", features: ["srt"] }, { name: "moonlight", installed: true, path: "/Applications/Moonlight.app", install_hint: "", features: [] }];
    if (cmd === "moonlight_start") return { running: true, pid: 3, command: "moonlight", started_at: null, exit_code: null, last_output: [] };
    if (cmd === "tools_status" && meshPath.noSrt)
      return [{ name: "ffmpeg", installed: true, path: "/opt/homebrew/bin/ffmpeg", install_hint: "brew install ffmpeg", features: [] }];
    if (cmd === "mesh_local" && meshPath.endpoint)
      return { installed: true, running: true, userspace: true, peers: [{ name: "laptop", addresses: ["10.77.0.1"], path: "direct", endpoint: meshPath.endpoint, rtt_ms: 212 }] };
    const path = (args?.request as { path?: string } | undefined)?.path;
    if (cmd === "agent_request" && path === "/v1/display" && meshPath.endpoint)
      return { receiver: { running: false, pid: null, command: null, started_at: null, last_output: [], exit_code: null }, receiver_config: null, receiver_url: null, mpv_installed: true, sunshine: { name: "sunshine", active_state: "active", enabled: true }, playback_active: false };
    if (cmd === "agent_request" && path === "/v1/display/receiver/start")
      return { receiver: { running: true, pid: 2, command: "mpv", started_at: null, last_output: [], exit_code: null }, receiver_config: null, receiver_url: "udp://10.77.0.1:5600", mpv_installed: true, sunshine: null, playback_active: false };
    if (cmd === "sender_start") return { running: true, pid: 1, command: "ffmpeg", started_at: null, exit_code: null, last_output: [] };
    if (cmd === "sender_status" && stopped.capture) return { running: false, pid: null, command: "ffmpeg", started_at: null, exit_code: -1, last_output: ["Screen capture sent no bytes after 5 seconds. Remove Vercelify in Screen & System Audio Recording, then allow it again."] };
    const { demoCall } = await import("../lib/demo");
    return demoCall(cmd, args ?? {});
  },
}));

it("shows a stopped capture's permission repair outside diagnostic details", async () => {
  const settings: CockpitSettings = { agent_url: "http://10.77.0.1:7420", agent_token: "test", hostname: "laptop" };
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  render(<ToastProvider><SessionProvider value={session}><DisplayCaster /></SessionProvider></ToastProvider>);
  expect(await screen.findByTestId("capture-error")).toBeVisible();
  expect(screen.getByTestId("capture-error")).toHaveTextContent("Screen & System Audio Recording");
});

it("turns remote desktop on from the app when Sunshine isn't set up on the server", async () => {
  const { fireEvent } = await import("@testing-library/react");
  const settings: CockpitSettings = { agent_url: "http://10.77.0.1:7420", agent_token: "test", hostname: "laptop" };
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  render(<ToastProvider><SessionProvider value={session}><DisplayCaster /></SessionProvider></ToastProvider>);
  const card = await screen.findByTestId("moonlight-card");
  expect(await screen.findByText("off")).toBeInTheDocument();
  expect(card).not.toHaveTextContent("not installed");
  fireEvent.click(await screen.findByTestId("desktop-on"));
  expect(await screen.findByText("Remote desktop is on", {}, { timeout: 4000 })).toBeInTheDocument();
});

it("extends the desktop: a new display is made and that screen is streamed", async () => {
  const { fireEvent } = await import("@testing-library/react");
  stopped.capture = false;
  sent.length = 0;
  const ua = vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 (Macintosh; Intel Mac OS X 15_0)");
  const settings: CockpitSettings = { agent_url: "http://10.77.0.1:7420", agent_token: "test", hostname: "laptop" };
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  render(<ToastProvider><SessionProvider value={session}><DisplayCaster /></SessionProvider></ToastProvider>);
  fireEvent.click(await screen.findByTestId("mode-extend"));
  fireEvent.click(await screen.findByTestId("start-extend"));
  await waitFor(() => expect(sent.some((c) => c.cmd === "sender_start")).toBe(true), { timeout: 4000 });
  const made = sent.find((c) => c.cmd === "extend_display_start");
  expect(made?.args).toMatchObject({ width: 1920, height: 1080, fps: 60 });
  const start = sent.find((c) => c.cmd === "sender_start");
  const config = start?.args?.config as { screen_index: number; width: number | null };
  expect(config.screen_index).toBe(4);
  // Sent at the new display's own size, not scaled back up to the quality's.
  expect(config.width).toBeNull();
  ua.mockRestore();
  stopped.capture = true;
});

it("warns that a moving picture breaks up over the internet, and offers Light", async () => {
  const { fireEvent } = await import("@testing-library/react");
  meshPath.endpoint = "49.43.246.18:51820";
  const settings: CockpitSettings = { agent_url: "http://10.77.0.1:7420", agent_token: "test", hostname: "laptop" };
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  render(<ToastProvider><SessionProvider value={session}><DisplayCaster /></SessionProvider></ToastProvider>);
  const warning = await screen.findByTestId("stream-path-warning", {}, { timeout: 4000 });
  expect(warning).toHaveTextContent("over the internet");
  expect(warning).toHaveTextContent("212 ms");
  fireEvent.click(screen.getByTestId("use-light"));
  expect(screen.getByTestId("quality")).toHaveValue("light");
  meshPath.endpoint = "";
});

it("says when this Mac's ffmpeg can't send SRT, instead of failing to start", async () => {
  const { fireEvent } = await import("@testing-library/react");
  meshPath.noSrt = true;
  sent.length = 0;
  stopped.capture = false;
  const ua = vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 (Macintosh; Intel Mac OS X 15_0)");
  const settings: CockpitSettings = { agent_url: "http://10.77.0.1:7420", agent_token: "test", hostname: "laptop" };
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  render(<ToastProvider><SessionProvider value={session}><DisplayCaster /></SessionProvider></ToastProvider>);
  fireEvent.click(await screen.findByTestId("display-advanced"));
  fireEvent.click(await screen.findByRole("radio", { name: "SRT" }));
  expect(await screen.findByText(/can't send SRT/)).toBeInTheDocument();
  fireEvent.click(await screen.findByTestId("start-extend"));
  expect(await screen.findAllByText(/can't send SRT/, {}, { timeout: 4000 })).not.toHaveLength(0);
  expect(sent.some((c) => c.cmd === "sender_start")).toBe(false);
  ua.mockRestore();
  stopped.capture = true;
  meshPath.noSrt = false;
});

it("launches Moonlight light, and says why, when the server is reached over the internet", async () => {
  const { fireEvent } = await import("@testing-library/react");
  meshPath.endpoint = "49.43.246.18:51820";
  sent.length = 0;
  localStorage.setItem("vercelify.moonlight.paired", JSON.stringify(["http://10.77.0.1:7420"]));
  const settings: CockpitSettings = { agent_url: "http://10.77.0.1:7420", agent_token: "test", hostname: "laptop" };
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  render(<ToastProvider><SessionProvider value={session}><DisplayCaster /></SessionProvider></ToastProvider>);
  const card = await screen.findByTestId("moonlight-card");
  await waitFor(() => expect(card.querySelector("[data-testid=moonlight-path-warning]")).not.toBeNull(), { timeout: 4000 });
  const launch = await screen.findByTestId("moonlight-launch");
  await waitFor(() => expect(launch).toBeEnabled(), { timeout: 4000 });
  fireEvent.click(launch);
  await waitFor(() => expect(sent.some((c) => c.cmd === "moonlight_start")).toBe(true), { timeout: 4000 });
  const launched = sent.find((c) => c.cmd === "moonlight_start")?.args?.launch;
  expect(launched).toMatchObject({ resolution: "1280x720", fps: 30, bitrate_kbps: 3000 });
  localStorage.removeItem("vercelify.moonlight.paired");
  meshPath.endpoint = "";
});
