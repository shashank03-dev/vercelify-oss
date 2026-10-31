import { cleanup, render, waitFor } from "@testing-library/react";
import { PairForm } from "./PairForm";

const api = vi.hoisted(() => ({
  settings: { joinPairing: vi.fn(), test: vi.fn(), save: vi.fn(), pendingPairing: vi.fn(), configNotice: vi.fn() },
  servers: { discover: vi.fn() },
  mesh: { discover: vi.fn() },
}));
vi.mock("../lib/api", () => ({ ...api, pairingToSettings: (p: unknown) => p }));
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.servers.discover.mockResolvedValue([]);
  api.mesh.discover.mockResolvedValue([]);
  api.settings.save.mockResolvedValue(undefined);
  api.settings.configNotice.mockResolvedValue(null);
});

it("says why the saved servers are gone when their file couldn't be read", async () => {
  api.settings.pendingPairing.mockResolvedValue(null);
  api.settings.configNotice.mockResolvedValue("This app's saved servers couldn't be read, so it starts fresh: pair again.");
  const { findByText } = render(<PairForm onPaired={vi.fn()} />);
  expect(await findByText(/couldn't be read/)).toBeInTheDocument();
});

it("a code that joined the network is saved even while the tunnel comes up (F75)", async () => {
  api.settings.pendingPairing.mockResolvedValue("vfy2.from-installer");
  api.settings.joinPairing.mockResolvedValue({ agent_url: "http://10.77.0.1:7420", hostname: "laptop", agent_token: "t", mesh_join: "vfyj1.x" });
  api.settings.test.mockRejectedValue(new Error("cannot reach"));
  const onPaired = vi.fn();
  render(<PairForm onPaired={onPaired} />);
  await waitFor(() => expect(onPaired).toHaveBeenCalled());
  expect(api.settings.save).toHaveBeenCalledWith(expect.objectContaining({ agent_token: "t" }));
  expect(onPaired.mock.calls[0]![1]).toMatch(/keeps trying/);
});

it("a plain code (nothing spent) still needs the server to answer", async () => {
  api.settings.pendingPairing.mockResolvedValue("vfy1.lan");
  api.settings.joinPairing.mockResolvedValue({ agent_url: "http://192.168.1.5:7420", hostname: "laptop", agent_token: "t" });
  api.settings.test.mockRejectedValue(new Error("cannot reach"));
  const onPaired = vi.fn();
  render(<PairForm onPaired={onPaired} />);
  await waitFor(() => expect(api.settings.test).toHaveBeenCalled());
  await new Promise((r) => setTimeout(r, 20));
  expect(onPaired).not.toHaveBeenCalled();
  expect(api.settings.save).not.toHaveBeenCalled();
});
