// The Vercelify environment on this computer: the host manager
// (`vercelify-host`) that lets a Mac or Windows PC host apps in one shared
// Linux environment. Calls go through the desktop app (host_request), which
// reads the owner token and only reaches /v1/host/… on the loopback.

import { call } from "./transport";
import type {
  ChangeEnvironment,
  ConfirmedChange,
  CreateEnvironment,
  EnvironmentPairing,
  EnvironmentRecommendation,
  EnvironmentState,
  HostLocal,
  HostOperation,
  ManagedEnvironment,
  SetupStage,
} from "../types";

function req<T>(method: "GET" | "POST" | "PATCH" | "DELETE", path: string, body?: unknown): Promise<T> {
  return call<T>("host_request", { method, path: `/v1/host${path}`, body: body ?? null });
}

/** A fresh idempotency key: a retried click replays, a new click is new. */
export function opKey(): string {
  return `ui-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

export const host = {
  local: () => call<HostLocal>("host_local"),
  recommendation: () => req<EnvironmentRecommendation>("POST", "/environment/recommendation"),
  environment: () => req<ManagedEnvironment>("GET", "/environment"),
  create: (b: CreateEnvironment) => req<HostOperation>("POST", "/environment", b),
  change: (b: ChangeEnvironment) => req<HostOperation>("PATCH", "/environment", b),
  start: () => req<HostOperation>("POST", "/environment/start", { idempotency_key: opKey() }),
  stop: (b: ConfirmedChange) => req<HostOperation>("POST", "/environment/stop", b),
  remove: (b: ConfirmedChange) => req<HostOperation>("DELETE", "/environment", b),
  operation: (id: string) => req<HostOperation>("GET", `/operations/${encodeURIComponent(id)}`),
  /** A main-server environment's one-use pairing code (through this computer). */
  pairing: () => req<EnvironmentPairing>("POST", "/environment/pairing"),
};

export const GIB = 1024 ** 3;

/** The owner-facing stages, in order (design §7 step 5). */
export const STAGES: { id: SetupStage; label: string }[] = [
  { id: "checking_computer", label: "Checking computer" },
  { id: "preparing_environment", label: "Preparing environment" },
  { id: "starting_hosting", label: "Starting hosting" },
  { id: "checking_connection", label: "Checking connection" },
];

export const STATE_LABEL: Record<EnvironmentState, string> = {
  not_created: "Not set up",
  preparing: "Preparing",
  starting: "Starting",
  verifying: "Checking",
  ready: "Running",
  stopping: "Stopping",
  stopped: "Stopped",
  updating: "Applying changes",
  removing: "Deleting",
  blocked: "Needs attention",
  failed: "Failed",
};

export function stateHealth(s: EnvironmentState): "ok" | "warn" | "bad" | "idle" {
  if (s === "ready") return "ok";
  if (s === "failed" || s === "blocked") return "bad";
  if (s === "stopped" || s === "not_created") return "idle";
  return "warn";
}

export function busy(s: EnvironmentState): boolean {
  return ["preparing", "starting", "verifying", "stopping", "updating", "removing"].includes(s);
}

/** Bytes as whole GB for an input, rounded down to 0.25. */
export function toGb(bytes: number): number {
  return Math.floor((bytes / GIB) * 4) / 4;
}

export function fromGb(gb: number): number {
  return Math.round(gb * 1024) * (GIB / 1024);
}
