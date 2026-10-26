import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { SessionProvider } from "../lib/session";
import type { CockpitSettings, DisplayStatus } from "../types";
import { DisplayCaster } from "./DisplayCaster";
import { ExplainFailure } from "./ExplainFailure";
import { ToastProvider } from "./Toast";

const wire = vi.hoisted(() => ({
  discoveryFinished: false, streams: 0, selected: "server-a", blocked: "", release: () => {},
  requests: [] as { host: string; path: string; method?: string }[],
}));
vi.mock("../lib/live", () => ({
  useLive: () => ({ asleep: new Set(), system: { data: undefined }, alerts: { data: undefined }, jobs: [], health: { data: { capabilities: [] } } }),
  useLiveTopic: () => {},
}));
vi.mock("../lib/nav", async (original) => ({
  ...await original<typeof import("../lib/nav")>(),
  useNav: () => ({ intent: null, clearIntent: () => {}, go: () => {} }),
}));
vi.mock("../lib/transport", async (original) => ({
  ...await original<typeof import("../lib/transport")>(),
  streamLines: () => { wire.streams++; return { done: Promise.resolve(), cancel: () => {} }; },
  call: async (cmd: string, args: { server?: string; request?: { path: string; method: string } } = {}) => {
    const path = args.request?.path ?? cmd;
    wire.requests.push({ host: args.server ?? wire.selected, path, method: args.request?.method });
    if (path === wire.blocked) {
      await new Promise<void>((resolve) => { wire.release = resolve; });
      if (path === "sender_start") throw new Error("screen capture denied");
    }
    const { demoCall } = await import("../lib/demo");
    if (path === "/v1/display/receiver/start") {
      const display = await demoCall("agent_request", { request: { path: "/v1/display", method: "GET" } }) as DisplayStatus;
      return { ...display, receiver: { ...display.receiver, running: true, pid: 123 }, receiver_url: "udp://100.64.0.1:5600" };
    }
    const response = await demoCall(cmd, { ...args, server: undefined });
    if (path === "/v1/llm") wire.discoveryFinished = true;
    return response;
  },
}));

const settings: CockpitSettings = { agent_url: "server-a", agent_token: "test", hostname: "A" };
function Page({ children }: { children: ReactNode }) {
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  return <ToastProvider><SessionProvider value={session}>{children}</SessionProvider></ToastProvider>;
}
beforeEach(() => { wire.discoveryFinished = false; wire.streams = 0; wire.selected = "server-a"; wire.requests = []; });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView"); });

it("cleans up the original display receiver if local capture fails after switching servers", async () => {
  wire.blocked = "sender_start";
  const page = render(<Page><DisplayCaster /></Page>);
  fireEvent.click(await screen.findByTestId("start-extend"));
  await waitFor(() => expect(wire.requests.some((r) => r.path === "sender_start")).toBe(true));
  page.unmount();
  wire.selected = "server-b";
  await act(async () => wire.release());
  await waitFor(() => expect(wire.requests.some((r) => r.path === "/v1/display/receiver/stop")).toBe(true));
  expect(wire.requests.find((r) => r.path === "/v1/display/receiver/stop")?.host).toBe("server-a");
});

it("does not start a build explanation after its panel closes during model discovery", async () => {
  wire.blocked = "/v1/llm";
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: () => {} });
  const lines = [{ output: "npm ERR! missing script: build", type: "stderr" }];
  const deployment = { application_name: "notes-api", commit: "abc1234", commit_message: "Add search" };
  const page = render(<Page><ExplainFailure deployment={deployment} lines={lines} /></Page>);
  fireEvent.click(await screen.findByTestId("explain-failure"));
  await waitFor(() => expect(wire.requests.some((r) => r.path === "/v1/llm")).toBe(true));
  page.unmount();
  await act(async () => wire.release());
  await waitFor(() => expect(wire.discoveryFinished).toBe(true));
  expect(wire.streams).toBe(0);
  Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
});
