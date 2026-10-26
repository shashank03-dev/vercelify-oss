import { act, fireEvent, render, screen, waitFor, cleanup } from "@testing-library/react";
import { ToastProvider } from "../components/Toast";
import { SessionProvider } from "../lib/session";
import type { CockpitSettings, Job, VolumeInfo } from "../types";
import { StorageView } from "./StorageView";

const wire = vi.hoisted(() => ({ selected: "server-a", requests: [] as { host: string; path: string; method: string }[], releaseMove: () => {} }));
vi.mock("../lib/live", () => ({ useLive: () => ({ health: { data: { capabilities: ["platform", "jobs"] } } }), useLiveTopic: () => {} }));
const volume: VolumeInfo = {
  owner_name: "production-db",
  volume: { id: "vol-42", owner_kind: "database", owner_id: "db-a", name: "data", mount_path: "/var/lib/postgresql/data", tier: "hdd", host_path: "/mnt/storage/volumes/db-a", created_at: "2026-10-23T00:00:00Z" },
};
const job: Job = { id: "move-a", kind: "volume-move", title: "Move database", status: "ok", started_at: "2026-10-23T00:00:00Z", finished_at: "2026-10-23T00:01:00Z", detail: "Copied", progress: 100, target: "vol-42" };
vi.mock("../lib/transport", () => ({
  call: async (_cmd: string, args: { server?: string; request: { path: string; method: string } }) => {
    const { path, method } = args.request;
    wire.requests.push({ host: args.server ?? wire.selected, path, method });
    if (path === "/v1/system") return { disks: [{ role: "storage", mount: "/mnt/storage", total_bytes: 1, used_bytes: 0, available_bytes: 1 }] };
    if (path === "/v1/platform/volumes") return [volume];
    if (path === "/v1/volumes") return [];
    if (path === "/v1/platform/volumes/vol-42/move") return new Promise((resolve) => { wire.releaseMove = () => resolve(job); });
    if (path === "/v1/jobs") return [job];
    throw new Error(`Unexpected request: ${path}`);
  },
}));

const settings: CockpitSettings = { agent_url: "server-a", agent_token: "test", hostname: "A" };
function Page() {
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  return <ToastProvider><SessionProvider value={session}><StorageView /></SessionProvider></ToastProvider>;
}

afterEach(cleanup);
it("follows a volume move on its original server after switching away", async () => {
  const page = render(<Page />);
  fireEvent.click(await screen.findByTestId("pmove-production-db-data"));
  fireEvent.click(screen.getByTestId("pmove-run"));
  await waitFor(() => expect(wire.requests.some((r) => r.method === "POST")).toBe(true));
  // Switching servers remounts the page, while the move's job is still followed.
  page.unmount();
  wire.selected = "server-b";
  await act(async () => wire.releaseMove());
  await waitFor(() => expect(wire.requests.some((r) => r.path === "/v1/jobs")).toBe(true));
  const move = wire.requests.filter((r) => r.method === "POST" || r.path === "/v1/jobs");
  expect(move.every((r) => r.host === "server-a")).toBe(true);
});
