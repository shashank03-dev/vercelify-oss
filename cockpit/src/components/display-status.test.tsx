import { render, screen } from "@testing-library/react";
import { SessionProvider } from "../lib/session";
import { ToastProvider } from "./Toast";
import { DisplayCaster } from "./DisplayCaster";
import type { CockpitSettings } from "../types";

vi.mock("../lib/live", () => ({ useLive: () => ({ health: { data: { capabilities: [] } } }) }));

vi.mock("../lib/transport", async (original) => ({
  ...await original<typeof import("../lib/transport")>(),
  call: async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "sender_status") return { running: false, pid: null, command: "ffmpeg", started_at: null, exit_code: -1, last_output: ["Screen capture sent no bytes after 5 seconds. Remove Vercelify in Screen & System Audio Recording, then allow it again."] };
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
