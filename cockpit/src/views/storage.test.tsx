import { act, fireEvent, render, screen, waitFor, cleanup } from "@testing-library/react";
import { ToastProvider } from "../components/Toast";
import { SessionProvider } from "../lib/session";
import type { CockpitSettings, CoolifyVolume, Job } from "../types";
import { StorageView } from "./StorageView";

const wire = vi.hoisted(() => ({ selected: "server-a", requests: [] as { host: string; path: string; method: string }[], background: false, releaseStop: () => {} }));
vi.mock("../lib/live", () => ({ useLive: () => ({ health: { data: { capabilities: wire.background ? ["jobs"] : [] } } }) }));
vi.mock("../lib/hooks", async (original) => ({ ...await original<typeof import("../lib/hooks")>(), sleep: () => Promise.resolve() }));
const volume: CoolifyVolume = {
  id: 42, name: "postgres-data", mount_path: "/var/lib/postgresql/data", host_path: null,
  tier: "hdd", data_path: "/mnt/storage/pg", resource_type: "StandalonePostgresql", resource_name: "Production DB",
  resource_uuid: "db-a", movable: true, in_use: false,
};
const job: Job = { id: "move-a", kind: "volume-move", title: "Move database", status: "ok", started_at: "2026-10-23T00:00:00Z", finished_at: "2026-10-23T00:01:00Z", detail: "Copied", progress: 100, target: "42" };
vi.mock("../lib/transport", () => ({
  call: async (_cmd: string, args: { server?: string; request: { path: string; method: string } }) => {
    const { path, method } = args.request;
    wire.requests.push({ host: args.server ?? wire.selected, path, method });
    if (path === "/v1/system") return { disks: [] };
    if (path === "/v1/coolify/volumes") return [volume];
    if (path === "/v1/volumes") return [];
    if (path === "/databases/db-a/stop") return new Promise<void>((resolve) => { wire.releaseStop = resolve; });
    if (path === "/databases") return [{ uuid: "db-a", status: "exited" }];
    if (path === "/v1/coolify/volumes/42/move?background=true") return job;
    if (path === "/v1/coolify/volumes/42/move") return { note: "Copied" };
    if (path === "/v1/jobs") return [job];
    if (path === "/databases/db-a/start") return {};
    throw new Error(`Unexpected request: ${path}`);
  },
}));

const settings: CockpitSettings = { agent_url: "server-a", agent_token: "test", coolify_url: "http://coolify-a", coolify_token: "test", hostname: "A" };
function Page() {
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  return <ToastProvider><SessionProvider value={session}><StorageView /></SessionProvider></ToastProvider>;
}

afterEach(cleanup);
for (const background of [false, true]) {
  it(`keeps a ${background ? "background" : "legacy"} volume move on its original server after switching away`, async () => {
    wire.selected = "server-a";
    wire.background = background;
    wire.requests = [];
    const page = render(<Page />);
    fireEvent.click(await screen.findByTestId("move-postgres-data"));
    fireEvent.click(screen.getByTestId("move-confirm"));
    await waitFor(() => expect(wire.requests.some((r) => r.path.endsWith("/stop"))).toBe(true));
    // Switching servers remounts LiveProvider and the page, while the async move continues.
    page.unmount();
    wire.selected = "server-b";
    await act(async () => wire.releaseStop());
    await waitFor(() => expect(wire.requests.some((r) => r.path.endsWith("/start"))).toBe(true));
    const move = wire.requests.filter((r) => r.method === "POST" || r.path === "/v1/jobs");
    expect(move.map((r) => r.host)).toEqual(background ? ["server-a", "server-a", "server-a", "server-a"] : ["server-a", "server-a", "server-a"]);
    expect(wire.requests.filter((r) => r.path === "/databases").every((r) => r.host === "server-a")).toBe(true);
  });
}
