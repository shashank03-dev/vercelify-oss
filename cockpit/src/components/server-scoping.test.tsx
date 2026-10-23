import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { SessionProvider } from "../lib/session";
import type { CockpitSettings, DisplayStatus } from "../types";
import { AppsView } from "../views/AppsView";
import { NewAppModal } from "./AutoDeploy";
import { DisplayCaster } from "./DisplayCaster";
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
    if (path === "/servers") return [{ uuid: "host-a", name: "localhost", ip: "host.docker.internal" }];
    if (path === "/projects") return [{ uuid: "project-a", name: "default" }];
    if (path === "/projects/project-a") return { environments: [{ uuid: "env-a", name: "production" }] };
    if (path === "/applications/public") return { uuid: "app-a" };
    if (path === "/v1/autodeploy" && args.request?.method === "POST") return { last_commit: "head" };
    if (path === "/databases/postgresql") return { uuid: "db-a" };
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

const settings: CockpitSettings = { agent_url: "server-a", agent_token: "test", coolify_url: "http://coolify-a", coolify_token: "test", hostname: "A" };
function Page({ children }: { children: ReactNode }) {
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  return <ToastProvider><SessionProvider value={session}>{children}</SessionProvider></ToastProvider>;
}
beforeEach(() => { wire.discoveryFinished = false; wire.streams = 0; wire.selected = "server-a"; wire.requests = []; });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView"); });

it("keeps app creation and auto-deploy setup on the server where creation began", async () => {
  wire.blocked = "/projects";
  const page = render(<Page><NewAppModal open onClose={() => {}} onCreated={() => {}} /></Page>);
  fireEvent.change(screen.getByTestId("new-app-repo"), { target: { value: "https://github.com/example/app" } });
  await waitFor(() => expect(wire.requests.some((r) => r.path === "/servers")).toBe(true));
  // Wait for the fetched server to become the selected placement target.
  await act(async () => {});
  fireEvent.submit(screen.getByTestId("new-app-repo").closest("form")!);
  await waitFor(() => expect(wire.requests.some((r) => r.path === "/projects")).toBe(true));
  page.unmount();
  wire.selected = "server-b";
  await act(async () => wire.release());
  await waitFor(() => expect(wire.requests.some((r) => r.path.endsWith("/check"))).toBe(true));
  const flow = wire.requests.filter((r) => r.path.startsWith("/projects") || r.method === "POST");
  expect(flow.every((r) => r.host === "server-a")).toBe(true);
});

it("keeps database creation on the original server after switching during project discovery", async () => {
  wire.blocked = "/projects";
  const page = render(<Page><AppsView /></Page>);
  fireEvent.click(await screen.findByTestId("new-database"));
  fireEvent.submit(screen.getByTestId("db-create").closest("form")!);
  await waitFor(() => expect(wire.requests.some((r) => r.path === "/projects")).toBe(true));
  page.unmount();
  wire.selected = "server-b";
  await act(async () => wire.release());
  await waitFor(() => expect(wire.requests.some((r) => r.path === "/databases/postgresql")).toBe(true));
  expect(wire.requests.find((r) => r.path === "/databases/postgresql")?.host).toBe("server-a");
});

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

it("does not start a build explanation after its dialog closes during model discovery", async () => {
  wire.blocked = "/v1/llm";
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: () => {} });
  const page = render(<Page><AppsView /></Page>);
  const app = await screen.findByRole("row", { name: /notes-api/ });
  fireEvent.click(within(app).getByRole("button", { name: "Deployments & logs" }));
  await screen.findAllByTestId("deployment-row");
  const failed = screen.getAllByTestId("deployment-row").find((row) => row.textContent?.includes("Error"))!;
  fireEvent.click(within(failed).getByRole("button", { name: "Build logs" }));
  fireEvent.click(await screen.findByTestId("explain-failure"));
  await waitFor(() => expect(wire.requests.some((r) => r.path === "/v1/llm")).toBe(true));
  page.unmount();
  await act(async () => wire.release());
  await waitFor(() => expect(wire.discoveryFinished).toBe(true));
  expect(wire.streams).toBe(0);
  Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
});
