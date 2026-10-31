// One entry point for every backend call.
//
// Inside the desktop app calls go to Rust commands (src-tauri/src/lib.rs).
// In a plain browser (dev server, end-to-end tests) the same commands are
// emulated here: HTTP goes through Vite's /__agent proxy and
// settings live in localStorage.

import { Channel, invoke } from "@tauri-apps/api/core";
import type { CockpitConfig } from "../types/generated/CockpitConfig";
import type { CockpitSettings } from "../types/generated/CockpitSettings";
import { decodePairing } from "./pairing";

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export class CockpitError extends Error {
  readonly status: number;
  constructor(message: string, status = 0) {
    super(message);
    this.name = "CockpitError";
    this.status = status;
  }
}

/** Rust errors arrive as "HTTP <status>: <message>" or plain strings. */
export function toCockpitError(e: unknown): CockpitError {
  if (e instanceof CockpitError) return e;
  const raw = typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
  const m = /^HTTP (\d{1,3}): ([\s\S]*)$/.exec(raw);
  return m ? new CockpitError(m[2] ?? raw, Number(m[1])) : new CockpitError(raw);
}

export interface HttpRequest {
  method: "GET" | "POST" | "PATCH" | "PUT" | "DELETE";
  path: string;
  body?: unknown;
  timeout?: number;
}

export async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return inTauri ? await invoke<T>(cmd, args) : await webCall<T>(cmd, args ?? {});
  } catch (e) {
    throw toCockpitError(e);
  }
}

/**
 * Call an agent endpoint that answers with NDJSON and receive each line as it
 * arrives (LLM chats). `cancel()` stops reading; the promise then resolves.
 */
export function streamLines(
  request: HttpRequest,
  onLine: (line: string) => void,
  server?: string,
): { done: Promise<void>; cancel: () => void } {
  const streamId = `s${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
  if (inTauri) {
    const channel = new Channel<string>();
    channel.onmessage = onLine;
    const done = invoke<void>("agent_stream", { request, server, streamId, onLine: channel }).catch((e) => {
      throw toCockpitError(e);
    });
    return { done, cancel: () => void invoke("stream_cancel", { streamId }).catch(() => {}) };
  }
  const controller = new AbortController();
  const done = (async () => {
    if (import.meta.env.VITE_DEMO) {
      const { demoStream } = await import("./demo");
      return demoStream(request, onLine, controller.signal);
    }
    const s = requireSettings(server);
    let resp: Response;
    try {
      resp = await fetch("/__agent" + request.path, {
        method: request.method,
        headers: { Authorization: `Bearer ${s.agent_token}`, "Content-Type": "application/json" },
        body: request.body !== undefined ? JSON.stringify(request.body) : undefined,
        signal: controller.signal,
      });
    } catch (e) {
      if (controller.signal.aborted) return;
      throw new CockpitError(`cannot reach the server (${(e as Error).message})`, 0);
    }
    if (!resp.ok || !resp.body) {
      const text = await resp.text();
      let msg = text;
      try {
        const v = JSON.parse(text) as { error?: string; message?: string };
        msg = v.error ?? v.message ?? text;
      } catch {
        /* not JSON */
      }
      throw new CockpitError(msg || resp.statusText, resp.status);
    }
    const reader = resp.body.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let i: number;
        while ((i = buf.indexOf("\n")) >= 0) {
          const line = buf.slice(0, i).trim();
          buf = buf.slice(i + 1);
          if (line) onLine(line);
        }
      }
    } catch (e) {
      if (!controller.signal.aborted) throw toCockpitError(e);
    }
    if (buf.trim()) onLine(buf.trim());
  })();
  return { done, cancel: () => controller.abort() };
}

// --- browser emulation ------------------------------------------------------

const CONFIG_KEY = "vercelify.servers";
/** Single-server settings written by 0.1. */
const LEGACY_KEY = "vercelify.settings";

function loadWebConfig(): CockpitConfig {
  try {
    const raw = localStorage.getItem(CONFIG_KEY);
    if (raw) return JSON.parse(raw) as CockpitConfig;
    const legacy = localStorage.getItem(LEGACY_KEY);
    if (legacy) {
      const s = JSON.parse(legacy) as CockpitSettings;
      return { servers: [s], active: s.agent_url };
    }
  } catch {
    /* corrupt or private mode */
  }
  return { servers: [], active: null };
}

function saveWebConfig(c: CockpitConfig) {
  localStorage.setItem(CONFIG_KEY, JSON.stringify(c));
  localStorage.removeItem(LEGACY_KEY);
}

/** Same rules as `CockpitConfig` in crates/vercelify-core/src/model.rs. */
export const configOps = {
  active(c: CockpitConfig): CockpitSettings | null {
    return c.servers.find((s) => s.agent_url === c.active) ?? c.servers[0] ?? null;
  },
  upsert(c: CockpitConfig, s: CockpitSettings): CockpitConfig {
    const servers = c.servers.some((x) => x.agent_url === s.agent_url)
      ? c.servers.map((x) => (x.agent_url === s.agent_url ? s : x))
      : [...c.servers, s];
    return { servers, active: s.agent_url };
  },
  remove(c: CockpitConfig, agentUrl: string): CockpitConfig {
    const servers = c.servers.filter((s) => s.agent_url !== agentUrl);
    const keep = c.active !== agentUrl && servers.some((s) => s.agent_url === c.active);
    return { servers, active: keep ? c.active : (servers[0]?.agent_url ?? null) };
  },
};

function loadWebSettings(): CockpitSettings | null {
  return configOps.active(loadWebConfig());
}

async function webHttp(base: string, token: string | null, req: HttpRequest): Promise<unknown> {
  const controller = new AbortController();
  const secs = req.timeout ?? 30;
  const timer = setTimeout(() => controller.abort(), secs * 1000);
  let resp: Response;
  let text: string;
  try {
    resp = await fetch(base + req.path, {
      method: req.method,
      headers: {
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
        Accept: "application/json",
        ...(req.body !== undefined ? { "Content-Type": "application/json" } : {}),
      },
      body: req.body !== undefined ? JSON.stringify(req.body) : undefined,
      signal: controller.signal,
    });
    text = await resp.text();
  } catch (e) {
    throw new CockpitError(controller.signal.aborted ? `the server didn't answer within ${secs}s` : `cannot reach the server (${(e as Error).message})`, 0);
  } finally {
    clearTimeout(timer);
  }
  let value: unknown = null;
  if (text.trim()) {
    try {
      value = JSON.parse(text);
    } catch {
      value = text;
    }
  }
  if (!resp.ok) {
    const obj = (value ?? {}) as { error?: string; message?: string };
    // The dev server's proxy answers by itself when the agent is down (the agent's own errors are JSON).
    if ((resp.status === 502 || resp.status === 504) && !obj.error && !obj.message) {
      throw new CockpitError("cannot reach the server — is it on and on the same network?", 0);
    }
    throw new CockpitError(obj.error ?? obj.message ?? (typeof value === "string" ? value : resp.statusText), resp.status);
  }
  return value;
}

function requireSettings(server?: unknown): CockpitSettings {
  const c = loadWebConfig();
  const s = typeof server === "string" ? c.servers.find((x) => x.agent_url === server) : configOps.active(c);
  if (!s) throw new CockpitError("Not paired yet");
  return s;
}

const DESKTOP_ONLY = "Available in the desktop app only";

async function webCall<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
  const r = (v: unknown) => v as T;
  // `VITE_DEMO=1 npm run dev`: canned data instead of a real host.
  if (import.meta.env.VITE_DEMO && !cmd.startsWith("settings_") && !cmd.startsWith("server") && cmd !== "pair_decode") {
    const { demoCall } = await import("./demo");
    // Calls without a server are for the selected one.
    return r(await demoCall(cmd, { ...args, server: args.server ?? loadWebConfig().active }));
  }
  switch (cmd) {
    case "settings_load":
      return r(loadWebSettings());
    case "settings_save":
      saveWebConfig(configOps.upsert(loadWebConfig(), args.settings as CockpitSettings));
      return r(null);
    case "settings_clear": {
      const c = loadWebConfig();
      const a = configOps.active(c);
      saveWebConfig(a ? configOps.remove(c, a.agent_url) : c);
      return r(null);
    }
    case "servers_load":
      return r(loadWebConfig());
    case "server_select": {
      const c = loadWebConfig();
      if (!c.servers.some((s) => s.agent_url === args.agentUrl)) throw new CockpitError(`${String(args.agentUrl)} is not a paired server`);
      const next = { ...c, active: String(args.agentUrl) };
      saveWebConfig(next);
      return r(next);
    }
    case "server_remove": {
      const next = configOps.remove(loadWebConfig(), String(args.agentUrl));
      saveWebConfig(next);
      return r(next);
    }
    case "pair_decode":
      return r(decodePairing(String(args.code ?? "")));
    case "pair_claim": {
      // Browser mode reaches one agent, through the dev-server proxy.
      const v = (await webHttp("/__agent", null, { method: "POST", path: "/v1/pair/claim", body: { code: args.code }, timeout: 15 })) as {
        pairing: string;
      };
      return r(decodePairing(v.pairing));
    }
    case "agent_request": {
      const s = requireSettings(args.server);
      return r(await webHttp("/__agent", s.agent_token, args.request as HttpRequest));
    }
    case "test_connection": {
      const s = args.settings as CockpitSettings;
      const sys = (await webHttp("/__agent", s.agent_token, { method: "GET", path: "/v1/system", timeout: 10 })) as {
        host: { hostname: string };
      };
      return r({ hostname: sys.host.hostname });
    }
    case "discover_servers":
      throw new CockpitError("Finding servers on your network needs the desktop app — paste a pairing code instead");
    case "config_notice":
    case "pair_pending":
      return r(null);
    case "pair_join": {
      const p = decodePairing(String(args.code ?? ""));
      if (p.mesh_join) throw new CockpitError("Joining Vercelify's network needs the desktop app (it runs vercelify-mesh)");
      return r(p);
    }
    case "mesh_local":
      return r({ installed: false, running: false, userspace: false, revoked: false, peers: [] });
    case "mesh_discover":
      return r([]);
    case "mesh_start":
      throw new CockpitError("Starting Vercelify's network needs the desktop app");
    case "mesh_pair":
      throw new CockpitError("Pairing over the LAN needs the desktop app — paste a pairing code instead");
    case "tools_status":
      return r([]);
    case "save_private_file": {
      // The browser's own download (its Downloads folder; it asks where if set to).
      const url = URL.createObjectURL(new Blob([String(args.text ?? "")], { type: "text/plain" }));
      const link = Object.assign(document.createElement("a"), { href: url, download: String(args.filename ?? "vercelify.txt") });
      link.click();
      URL.revokeObjectURL(url);
      return r("your browser's downloads");
    }
    case "capture_screens":
      return r([]);
    case "sender_status":
    case "moonlight_status":
      return r({ running: false, pid: null, command: null, started_at: null, last_output: [], exit_code: null });
    default:
      throw new CockpitError(DESKTOP_ONLY);
  }
}
