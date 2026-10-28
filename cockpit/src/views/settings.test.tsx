import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SettingsView } from "./SettingsView";
import { ToastProvider } from "../components/Toast";
const api = vi.hoisted(() => ({
  joinPairing: vi.fn(), test: vi.fn(), update: vi.fn(),
}));
vi.mock("../lib/api", () => ({ settings: api, pairingToSettings: (p: unknown) => p, local: { tools: async () => [] } }));
vi.mock("../lib/session", () => ({ useSession: () => ({ settings: { agent_url: "a", hostname: "A", agent_token: "old" }, servers: [], update: api.update, unpair: vi.fn() }) }));
vi.mock("../lib/nav", () => ({ useNav: () => ({ theme: "dark", setTheme: vi.fn() }) }));
vi.mock("../components/ServerCare", () => ({ AlertsCard: () => null, AuditCard: () => null, BackupsCard: () => null, DevicesCard: () => null, DomainCard: () => null, PowerGuardCard: () => null }));
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.joinPairing.mockResolvedValue({ agent_url: "b", hostname: "B", agent_token: "new" });
  api.test.mockResolvedValue({ hostname: "B" });
  api.update.mockResolvedValue(undefined);
});
it("joins the mesh before testing and saving a replacement pairing", async () => {
  render(<ToastProvider><SettingsView /></ToastProvider>);
  fireEvent.click(screen.getByTestId("settings-computer"));
  fireEvent.change(await screen.findByPlaceholderText("vfy2.…"), { target: { value: "vfy2.new-code" } });
  fireEvent.click(screen.getByRole("button", { name: "Apply" }));
  await waitFor(() => expect(api.update).toHaveBeenCalledWith({ agent_url: "b", hostname: "B", agent_token: "new" }));
  expect(api.joinPairing).toHaveBeenCalledWith("vfy2.new-code");
  expect(api.joinPairing.mock.invocationCallOrder[0]).toBeLessThan(api.test.mock.invocationCallOrder[0]!);
});
it("keeps the code available for retry when connection testing fails", async () => {
  api.test.mockRejectedValue(new Error("unreachable"));
  render(<ToastProvider><SettingsView /></ToastProvider>);
  fireEvent.click(screen.getByTestId("settings-computer"));
  fireEvent.change(await screen.findByPlaceholderText("vfy2.…"), { target: { value: "vfy2.retry-code" } });
  fireEvent.click(screen.getByRole("button", { name: "Apply" }));
  await waitFor(() => expect(screen.getByText("unreachable")).toBeInTheDocument());
  expect(screen.getByPlaceholderText("vfy2.…")).toHaveValue("vfy2.retry-code");
  expect(api.update).not.toHaveBeenCalled();
});
