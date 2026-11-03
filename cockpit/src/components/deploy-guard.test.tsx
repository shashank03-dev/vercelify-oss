import { beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { DeployGuardCard } from "./DeployGuard";
import { platformRoute } from "../lib/demo-platform";
import type { PlatformClient } from "../lib/platform";
import type { App } from "../types";
const gate = vi.hoisted(() => ({ caps: ["deployment-guards"] as string[] }));
vi.mock("../lib/live", () => ({ useLive: () => ({ health: { data: { capabilities: gate.caps } } }) }));
vi.mock("./Toast", () => ({ useToast: () => ({ ok: vi.fn(), bad: vi.fn() }) }));
beforeEach(() => { gate.caps = ["deployment-guards"]; });
function fixture(state = "requested") {
  const app = platformRoute({ method: "GET", path: "/v1/platform/apps/shop" }) as App;
  const patchApp = vi.fn(async () => app);
  const deploymentGuards = vi.fn(async () => [{ deployment_id: "new", state, reason: "", rollback_id: "old" }]);
  return { app, patchApp, deploymentGuards, client: { patchApp, deploymentGuards } as unknown as PlatformClient };
}
it("requires explicit migration compatibility and never calls a queued rollback verified", async () => {
  const f = fixture(); render(<DeployGuardCard app={f.app} client={f.client} onChanged={vi.fn()} />);
  expect(screen.getByTestId("deploy-guard-enabled")).not.toBeChecked();
  expect(await screen.findByText("Rollback requested · verification pending")).toBeVisible();
  expect(screen.queryByText("Previous version verified")).not.toBeInTheDocument();
  fireEvent.click(screen.getByTestId("deploy-guard-enabled"));
  fireEvent.change(screen.getByLabelText("Rollback startup grace"), { target: { value: "20" } });
  fireEvent.click(screen.getByTestId("save-deploy-guard"));
  await waitFor(() => expect(f.patchApp).toHaveBeenCalledWith(f.app.id, { runtime: { ...f.app.runtime, rollback_compatible: true, rollback_grace_secs: 20, rollback_failures: 3 } }));
});
it("only labels verified recovery as the previous version serving", async () => {
  const f = fixture("verified"); render(<DeployGuardCard app={f.app} client={f.client} onChanged={vi.fn()} />);
  expect(await screen.findByText("Previous version verified")).toBeVisible();
});
it("does not write new policy to a server without the capability", () => {
  gate.caps = []; const f = fixture(); render(<DeployGuardCard app={f.app} client={f.client} onChanged={vi.fn()} />);
  expect(screen.getByText("Deploy recovery needs a server upgrade")).toBeVisible();
  expect(f.deploymentGuards).not.toHaveBeenCalled();
  expect(screen.queryByTestId("save-deploy-guard")).not.toBeInTheDocument();
});
