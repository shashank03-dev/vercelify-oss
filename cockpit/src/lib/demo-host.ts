// `VITE_DEMO=1`: a host manager for this computer, so the Vercelify
// environment flow can be tried and tested without a VM. It walks the same
// stages and refuses the same things as `vercelify-host` (manager.rs).

import type {
  EnvironmentGrant,
  EnvironmentRecommendation,
  HostLocal,
  HostOperation,
  ManagedEnvironment,
  OperationKind,
  SetupStage,
} from "../types";

const GIB = 1024 ** 3;
const MIB = 1024 ** 2;

const recommendation: EnvironmentRecommendation = {
  grant: { memory_bytes: 6 * GIB, cpus: 6, disk_bytes: 64 * GIB, policy_version: 1 },
  max: { memory_bytes: 10 * GIB + 256 * MIB, cpus: 6, disk_bytes: 180 * GIB, policy_version: 1 },
  min: { memory_bytes: 2 * GIB, cpus: 1, disk_bytes: 20 * GIB, policy_version: 1 },
  blockers: [],
  warnings: ["this computer is on battery: new builds wait while the battery is low, and a sleeping computer serves nothing"],
  measured_at: 1_800_000_000,
  provider_overhead_bytes: 256 * MIB,
};

let env: ManagedEnvironment = {
  id: "env_demo",
  state: "not_created",
  revision: 0,
  provider: "lima-vz",
  grant: null,
  image: "",
  reason: null,
  retry_safe: true,
  last_stage: null,
  operation: null,
  role: null,
};
const ops = new Map<string, HostOperation>();
const byKey = new Map<string, string>();

class HttpError extends Error {
  constructor(status: number, message: string) {
    super(`HTTP ${status}: ${message}`);
  }
}

function within(g: EnvironmentGrant): string[] {
  const max = recommendation.max!;
  const min = recommendation.min;
  const errs: string[] = [];
  if (g.memory_bytes < min.memory_bytes || g.memory_bytes > max.memory_bytes) errs.push("memory must be between 2 GB and 10.2 GB");
  if (g.cpus < min.cpus || g.cpus > max.cpus) errs.push(`processing power must be between ${min.cpus} and ${max.cpus} processors`);
  if (g.disk_bytes < min.disk_bytes || g.disk_bytes > max.disk_bytes) errs.push("the storage limit must be between 20 GB and 180 GB");
  return errs;
}

function begin(key: string, kind: OperationKind, next: ManagedEnvironment["state"], update: (e: ManagedEnvironment) => void): HostOperation {
  const known = byKey.get(key);
  if (known) return ops.get(known)!;
  const running = env.operation ? ops.get(env.operation) : undefined;
  if (running?.status === "running") throw new HttpError(409, `another change (${running.kind}) is in progress: wait for it to finish`);
  const op: HostOperation = { id: `op_${Math.random().toString(36).slice(2, 10)}`, kind, key, status: "running", stage: null, error: null, created_at: Date.now(), finished_at: null };
  ops.set(op.id, op);
  byKey.set(key, op.id);
  env = { ...env, state: next, revision: env.revision + 1, operation: op.id, reason: null };
  update(env);
  return op;
}

/** Steps a few seconds apart, like a fast real run. */
function play(op: HostOperation, steps: [SetupStage | null, ManagedEnvironment["state"]][], end: ManagedEnvironment["state"]) {
  steps.forEach(([stage, state], i) =>
    setTimeout(() => {
      env = { ...env, state, last_stage: stage ?? env.last_stage };
      ops.set(op.id, { ...ops.get(op.id)!, stage });
    }, 900 * (i + 1)),
  );
  setTimeout(() => {
    env = end === "not_created" ? { ...env, id: "env_demo2", state: end, grant: null, last_stage: null } : { ...env, state: end };
    ops.set(op.id, { ...ops.get(op.id)!, status: "succeeded", finished_at: Date.now() });
  }, 900 * (steps.length + 1));
}

function revision(r: unknown) {
  if (r !== env.revision) throw new HttpError(409, `the environment changed since you looked (revision ${env.revision} now, not ${String(r)}): look again`);
}

export function demoHostLocal(): HostLocal {
  return { installed: true, running: true, environment: { ...env }, error: null };
}

export function demoHostRequest(method: string, path: string, body: Record<string, unknown> | null): unknown {
  const b = body ?? {};
  switch (`${method} ${path}`) {
    case "POST /v1/host/environment/recommendation":
      return recommendation;
    case "GET /v1/host/environment":
      return { ...env };
    case "POST /v1/host/environment": {
      if (!["not_created", "failed", "blocked"].includes(env.state)) throw new HttpError(409, "this computer has its Vercelify environment already: new apps use it");
      const g = b.grant as EnvironmentGrant;
      const errs = within(g);
      if (errs.length) throw new HttpError(409, `this computer changed since you chose: ${errs.join("; ")}`);
      const join = b.join as string | null | undefined;
      if (join && !/^vfyj1\.[A-Za-z0-9._-]+$/.test(join)) throw new HttpError(400, "the invite should look like vfyj1.…");
      const op = begin(String(b.idempotency_key), "create", "preparing", (e) => {
        e.grant = g;
        e.image = "debian-12@20261026-2643";
        e.last_stage = "checking_computer";
        e.role = join ? "worker" : "main";
      });
      play(op, [["checking_computer", "preparing"], ["preparing_environment", "preparing"], ["starting_hosting", "starting"], ["checking_connection", "verifying"]], "ready");
      return op;
    }
    case "PATCH /v1/host/environment": {
      revision(b.revision);
      if (env.state === "ready" && !b.restart_ok) throw new HttpError(409, "this restarts the environment: its apps stop for a minute or two. Confirm to go ahead");
      const g = b.grant as EnvironmentGrant;
      if (env.grant && g.disk_bytes < env.grant.disk_bytes) throw new HttpError(400, "the storage limit can't be made smaller");
      const errs = within(g);
      if (errs.length) throw new HttpError(400, errs.join("; "));
      const wasReady = env.state === "ready";
      const op = begin(String(b.idempotency_key), "resize", "updating", (e) => (e.grant = g));
      play(op, wasReady ? [["starting_hosting", "starting"], ["checking_connection", "verifying"]] : [], wasReady ? "ready" : "stopped");
      return op;
    }
    case "POST /v1/host/environment/start": {
      const op = begin(String(b.idempotency_key), "start", "starting", () => {});
      play(op, [["starting_hosting", "starting"], ["checking_connection", "verifying"]], "ready");
      return op;
    }
    case "POST /v1/host/environment/stop": {
      if (!b.acknowledged) throw new HttpError(400, "stopping the environment stops every app on this computer: confirm it");
      revision(b.revision);
      const op = begin(String(b.idempotency_key), "stop", "stopping", () => {});
      play(op, [], "stopped");
      return op;
    }
    case "DELETE /v1/host/environment": {
      if (!b.acknowledged) throw new HttpError(400, "deleting the environment deletes every app, database and file in it: confirm it");
      revision(b.revision);
      const op = begin(String(b.idempotency_key), "remove", "removing", () => {});
      play(op, [], "not_created");
      return op;
    }
  }
  if (`${method} ${path}` === "POST /v1/host/environment/pairing") {
    if (env.state !== "ready") throw new HttpError(409, "start the environment first");
    if (env.role !== "main") throw new HttpError(400, "this environment joined another main server: pair with that one");
    return { code: "vfy1.eyJhZ2VudF91cmwiOiJodHRwOi8vMTkyLjE2OC4xLjQ0OjcwMjAifQ", address: "192.168.1.44" };
  }
  const m = /^GET \/v1\/host\/operations\/(.+)$/.exec(`${method} ${path}`);
  if (m) {
    const op = ops.get(decodeURIComponent(m[1]!));
    if (!op) throw new HttpError(404, `no operation ${m[1]}`);
    return op;
  }
  throw new HttpError(404, `no ${method} ${path}`);
}
