import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AwayLine, LocalLine } from "./MeshNetwork";

const api = vi.hoisted(() => ({ mesh: { start: vi.fn() }, agentOn: vi.fn() }));
vi.mock("../lib/api", () => api);
afterEach(cleanup);

const local = { installed: true, running: false, userspace: true, revoked: false, name: "mac", ipv4: "10.77.0.2", peers: [] };

it("a stopped network daemon can be started from the card (F59)", async () => {
  api.mesh.start.mockResolvedValue(undefined);
  render(<LocalLine local={local} />);
  expect(screen.getByText(/isn't running/)).toBeInTheDocument();
  fireEvent.click(screen.getByTestId("mesh-start"));
  await waitFor(() => expect(api.mesh.start).toHaveBeenCalledTimes(1));
});

it("says why it couldn't start", async () => {
  api.mesh.start.mockRejectedValue(new Error("vercelify-mesh isn't set up on this Mac: run install-mac.sh again"));
  render(<LocalLine local={local} />);
  fireEvent.click(screen.getByTestId("mesh-start"));
  expect(await screen.findByText(/run install-mac.sh again/)).toBeInTheDocument();
});

it("no button while it runs", () => {
  render(<LocalLine local={{ ...local, running: true }} />);
  expect(screen.queryByTestId("mesh-start")).toBeNull();
});

it("says when the network only works at home, and how to change that (review F13)", () => {
  const { rerender } = render(<AwayLine away={false} />);
  expect(screen.getByText("This network only")).toBeInTheDocument();
  expect(screen.getByText("sudo vercelify remote on")).toBeInTheDocument();
  rerender(<AwayLine away />);
  expect(screen.getByText("Ready away from home")).toBeInTheDocument();
  rerender(<AwayLine away={undefined} />);
  expect(screen.queryByTestId("mesh-away")).toBeNull();
});
