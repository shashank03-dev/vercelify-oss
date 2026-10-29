import { call } from "./transport";

afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); localStorage.clear(); });
it("keeps the request deadline active until the response body is complete", async () => {
  vi.useFakeTimers();
  localStorage.setItem("vercelify.servers", JSON.stringify({ servers: [{ agent_url: "http://server", agent_token: "token", hostname: "server" }], active: "http://server" }));
  let signal!: AbortSignal;
  let rejectBody!: (error: Error) => void;
  vi.stubGlobal("fetch", vi.fn(async (_url: string, options: RequestInit) => {
    signal = options.signal as AbortSignal;
    return { ok: true, text: () => new Promise<string>((_resolve, reject) => {
      rejectBody = reject;
      signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")));
    }) };
  }));
  const response = call("agent_request", { request: { method: "GET", path: "/v1/system", timeout: 1 } }).catch((e: Error) => e);
  await vi.advanceTimersByTimeAsync(1001);
  try {
    expect(signal.aborted).toBe(true);
    expect((await response as Error).message).toContain("within 1s");
  } finally { rejectBody(new Error("test finished")); }
});
