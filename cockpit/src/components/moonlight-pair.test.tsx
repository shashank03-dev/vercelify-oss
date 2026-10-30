import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { SessionProvider } from "../lib/session";
import type { CockpitSettings } from "../types";
import { DisplayCaster, randomPin } from "./DisplayCaster";
import { ToastProvider } from "./Toast";

// Launch Moonlight pairs it with the server's Sunshine first: Moonlight gets a
// PIN, the agent hands Sunshine the same one, nobody types it.
const wire = vi.hoisted(() => ({
  caps: ["sunshine-pair"] as string[],
  refuse: "",
  calls: [] as { cmd: string; args: Record<string, unknown> }[],
}));
vi.mock("../lib/live", () => ({
  useLive: () => ({ asleep: new Set(), system: { data: undefined }, alerts: { data: undefined }, jobs: [], health: { data: { capabilities: wire.caps } } }),
  useLiveTopic: () => {},
}));
vi.mock("../lib/transport", async (original) => ({
  ...await original<typeof import("../lib/transport")>(),
  call: async (cmd: string, args: Record<string, unknown> = {}) => {
    const request = args.request as { path: string; body?: unknown } | undefined;
    const name = request?.path ?? cmd;
    wire.calls.push({ cmd: name, args: (request?.body as Record<string, unknown>) ?? args });
    if (name === "/v1/display/sunshine/pair" && wire.refuse) throw new Error(wire.refuse);
    if (name === "tools_status") return [{ name: "moonlight", installed: true, path: "/Applications/Moonlight.app", install_hint: "" }];
    const { demoCall } = await import("../lib/demo");
    return demoCall(cmd, { ...args, server: undefined });
  },
}));

const settings: CockpitSettings = { agent_url: "http://10.77.0.1:7420", agent_token: "test", hostname: "laptop" };
function Page({ children }: { children: ReactNode }) {
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  return <ToastProvider><SessionProvider value={session}>{children}</SessionProvider></ToastProvider>;
}
const names = () => wire.calls.map((c) => c.cmd).filter((c) => c.startsWith("moonlight_") && c !== "moonlight_status" || c === "/v1/display/sunshine/pair");

beforeAll(async () => { await import("../lib/demo"); });
beforeEach(() => { wire.caps = ["sunshine-pair"]; wire.refuse = ""; wire.calls = []; localStorage.clear(); });
afterEach(cleanup);

async function launch() {
  render(<Page><DisplayCaster /></Page>);
  const button = await screen.findByRole("button", { name: /Launch Moonlight/ });
  await waitFor(() => expect(button).toBeEnabled());
  fireEvent.click(button);
}

it("pairs on the first launch with one PIN on both sides, then streams", async () => {
  await launch();
  await waitFor(() => expect(names()).toContain("moonlight_start"));
  expect(names()).toEqual(["moonlight_pair", "/v1/display/sunshine/pair", "moonlight_stop", "moonlight_start"]);
  const pair = wire.calls.find((c) => c.cmd === "moonlight_pair")!.args;
  const handed = wire.calls.find((c) => c.cmd === "/v1/display/sunshine/pair")!.args;
  expect(pair.pin).toMatch(/^\d{4}$/);
  expect(handed.pin).toBe(pair.pin);
  expect(await screen.findByText("Moonlight is paired with this server")).toBeInTheDocument();
  cleanup();

  // Remembered: the next launch goes straight to the desktop.
  wire.calls = [];
  await launch();
  await waitFor(() => expect(names()).toContain("moonlight_start"));
  expect(names()).toEqual(["moonlight_start"]);
});

it("doesn't launch when Sunshine refuses, and closes the pairing window", async () => {
  wire.refuse = "Sunshine already has a login that Vercelify didn't make. Reset it with `sudo vercelify desktop reset-login`";
  await launch();
  expect(await screen.findByText(/desktop reset-login/)).toBeInTheDocument();
  expect(names()).toEqual(["moonlight_pair", "/v1/display/sunshine/pair", "moonlight_stop"]);
  expect(localStorage.getItem("vercelify.moonlight.paired")).toBeNull();
});

it("an older server: launches as before, with the manual steps shown", async () => {
  wire.caps = [];
  await launch();
  await waitFor(() => expect(names()).toContain("moonlight_start"));
  expect(names()).toEqual(["moonlight_start"]);
  expect(screen.queryByTestId("moonlight-pair")).not.toBeInTheDocument();
  expect(screen.getByText(/Update the server to have this done for you/)).toBeInTheDocument();
});

it("Pair again pairs without launching", async () => {
  render(<Page><DisplayCaster /></Page>);
  const again = await screen.findByTestId("moonlight-pair");
  await waitFor(() => expect(again).toBeEnabled());
  fireEvent.click(again);
  await waitFor(() => expect(names()).toContain("moonlight_stop"));
  expect(names()).toEqual(["moonlight_pair", "/v1/display/sunshine/pair", "moonlight_stop"]);
});

it("PINs are four digits", () => {
  for (let i = 0; i < 200; i++) expect(randomPin()).toMatch(/^\d{4}$/);
});
