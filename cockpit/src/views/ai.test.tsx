import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ToastProvider } from "../components/Toast";
import { SessionProvider } from "../lib/session";
import type { CockpitSettings, LlmStatus } from "../types";
import { AiView } from "./AiView";

const stream = vi.hoisted(() => ({ active: false, finish: () => {} }));
const status: LlmStatus = {
  installed: true, running: true, ready: true, container_state: "running", image: "ollama/ollama", version: "test", port: 11434,
  endpoints: [], data_path: "/models", models: [{ name: "gemma4:e2b", size_bytes: 7_000_000_000, modified_at: null, family: "gemma", parameter_size: "2B", quantization: "Q4" }],
  loaded: [], tasks: [], firewall_active: true, memory_total_bytes: 16_000_000_000, memory_available_bytes: 10_000_000_000, error: null,
};
vi.mock("../lib/transport", async (original) => ({
  ...await original<typeof import("../lib/transport")>(),
  call: async (_cmd: string, args: { request: { path: string } }) => {
    if (args.request.path === "/v1/llm") return status;
    if (args.request.path === "/v1/system") return { disks: [] };
    throw new Error(`Unexpected request: ${args.request.path}`);
  },
  streamLines: () => {
    stream.active = true;
    const done = new Promise<void>((resolve) => { stream.finish = resolve; });
    return { done, cancel: () => { stream.active = false; stream.finish(); } };
  },
}));

it("cancels model generation when leaving the AI page", async () => {
  // jsdom has no layout/scrolling implementation.
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: () => {} });
  const settings: CockpitSettings = { agent_url: "http://server-a", agent_token: "test", coolify_url: "http://coolify-a", coolify_token: "test", hostname: "A" };
  const session = { settings, servers: [settings], unpair: async () => {}, update: async () => {}, select: async () => {}, remove: async () => {}, addServer: () => {}, revision: 0 };
  const page = render(<ToastProvider><SessionProvider value={session}><AiView /></SessionProvider></ToastProvider>);
  fireEvent.change(await screen.findByTestId("chat-input"), { target: { value: "Explain my build" } });
  fireEvent.click(screen.getByTestId("chat-send"));
  await waitFor(() => expect(stream.active).toBe(true));
  page.unmount();
  expect(stream.active).toBe(false);
  Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
});
