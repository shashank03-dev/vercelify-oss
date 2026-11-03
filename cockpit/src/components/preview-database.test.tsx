import { beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { PreviewDatabaseCard } from "./PreviewDatabase";
import { platformRoute } from "../lib/demo-platform";
import type { PlatformClient } from "../lib/platform";
import type { App } from "../types";

const gate = vi.hoisted(() => ({ caps: ["preview-databases"] as string[] }));
vi.mock("../lib/live", () => ({ useLive: () => ({ health: { data: { capabilities: gate.caps } } }) }));
vi.mock("./Toast", () => ({ useToast: () => ({ ok: vi.fn(), bad: vi.fn() }) }));
beforeEach(() => { gate.caps = ["preview-databases"]; });
function fixture() {
  const app = platformRoute({ method: "GET", path: "/v1/platform/apps/shop" }) as App;
  const patchApp = vi.fn(async () => app);
  const previewDatabases = vi.fn(async () => [{ id: "owned", pr_number: 7, state: "ready", error: "" }]);
  const client = { patchApp, previewDatabases } as unknown as PlatformClient;
  return { app, client, patchApp, previewDatabases };
}
it("requires opt-in and saves runtime-only migration and synthetic seed configuration", async () => {
  const f = fixture();
  render(<PreviewDatabaseCard app={f.app} client={f.client} onChanged={vi.fn()} />);
  expect(screen.getByTestId("preview-database-enabled")).not.toBeChecked();
  fireEvent.click(screen.getByTestId("preview-database-enabled"));
  fireEvent.change(screen.getByLabelText("Migration command"), { target: { value: "npm run migrate" } });
  fireEvent.change(screen.getByLabelText("Synthetic seed command"), { target: { value: "npm run seed:preview" } });
  fireEvent.click(screen.getByTestId("save-preview-database"));
  await waitFor(() => expect(f.patchApp).toHaveBeenCalledWith(f.app.id, { previews: { ...f.app.previews, max: 3,
    database: { variable: "DATABASE_URL", migration: "npm run migrate", seed: "npm run seed:preview" } } }));
  expect(await screen.findByText("Ready")).toBeVisible();
  expect(screen.queryByLabelText(/password/i)).not.toBeInTheDocument();
});
it("does not submit unsupported settings to an older server", () => {
  gate.caps = [];
  const f = fixture();
  render(<PreviewDatabaseCard app={f.app} client={f.client} onChanged={vi.fn()} />);
  expect(screen.getByText(/Update this server/)).toBeVisible();
  expect(f.previewDatabases).not.toHaveBeenCalled();
  expect(screen.queryByTestId("save-preview-database")).not.toBeInTheDocument();
});
it("disabling requests owned cleanup through app settings", async () => {
  const f = fixture(); f.app.previews.database = { variable: "DATABASE_URL" };
  render(<PreviewDatabaseCard app={f.app} client={f.client} onChanged={vi.fn()} />);
  fireEvent.click(screen.getByTestId("preview-database-enabled"));
  fireEvent.click(screen.getByTestId("save-preview-database"));
  await waitFor(() => expect(f.patchApp).toHaveBeenCalledWith(f.app.id, { previews: { ...f.app.previews, database: undefined } }));
});
