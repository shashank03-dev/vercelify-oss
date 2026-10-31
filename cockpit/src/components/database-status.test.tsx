import { render, screen } from "@testing-library/react";
import type { PlatformClient } from "../lib/platform";
import { DatabaseStatus } from "./DatabaseStatus";

it("does not call a missing database container running while its image downloads", async () => {
  const client = { databaseStatus: async () => ({ state: "missing", operation: { status: "running", detail: "Downloading image" } }) } as unknown as PlatformClient;
  render(<DatabaseStatus id="db" client={client} />);
  expect(await screen.findByText("Starting")).toBeInTheDocument();
  expect(screen.queryByText("Running")).not.toBeInTheDocument();
});

it("shows a stopped container even when desired state is running", async () => {
  const client = { databaseStatus: async () => ({ state: "exited", operation: null }) } as unknown as PlatformClient;
  render(<DatabaseStatus id="db" client={client} />);
  expect(await screen.findByText("Stopped")).toBeInTheDocument();
});
