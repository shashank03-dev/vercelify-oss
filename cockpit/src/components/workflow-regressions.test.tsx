import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { ToastProvider } from "./Toast";
import { StorageToggle } from "./StorageToggle";
import { GithubAppModal } from "./GitIntegrations";
import { Modal } from "./ui";
import type { PlatformClient } from "../lib/platform";

vi.mock("../lib/open", () => ({ openExternal: async () => {} }));
const live = vi.hoisted(() => ({ changed: () => {} }));
vi.mock("../lib/live", () => ({ useLiveTopic: (_: string, fn: () => void) => { live.changed = fn; } }));

it("does not offer an absent storage disk", () => {
  render(<ToastProvider><StorageToggle disks={[]} /></ToastProvider>);
  expect(screen.getByTestId("tier-hdd")).toBeDisabled();
});

it("finishes GitHub setup when its callback creates the source", async () => {
  let sources: unknown[] = [];
  const onCreated = vi.fn();
  const onClose = vi.fn();
  const client = { gitSources: async () => sources, startGithubApp: async () => ({ state: "s", start_url: "https://github.com/new", expires_at: new Date(Date.now() + 60_000).toISOString() }) } as unknown as PlatformClient;
  render(<ToastProvider><GithubAppModal client={client} onCreated={onCreated} onClose={onClose} /></ToastProvider>);
  fireEvent.click(screen.getByTestId("github-app-start"));
  await screen.findByTestId("github-app-finish");
  sources = [{ id: "source-new", name: "My App", auth: "github_app" }];
  live.changed();
  await waitFor(() => expect(onCreated).toHaveBeenCalledWith(sources[0]));
  expect(onClose).toHaveBeenCalled();
});

it("keeps keyboard focus in a modal and restores the opener on close", async () => {
  function Example() {
    const [open, setOpen] = useState(false);
    return <><button onClick={() => setOpen(true)}>Open</button><Modal open={open} title="Example" onClose={() => setOpen(false)}><input aria-label="Name" autoFocus /><button>Last</button></Modal></>;
  }
  const user = userEvent.setup();
  render(<Example />);
  const opener = screen.getByRole("button", { name: "Open" });
  await user.click(opener);
  const dialog = screen.getByRole("dialog");
  screen.getByRole("button", { name: "Last" }).focus();
  await user.tab();
  expect(dialog.contains(document.activeElement)).toBe(true);
  await user.keyboard("{Escape}");
  await waitFor(() => expect(opener).toHaveFocus());
});
