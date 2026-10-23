// Pooling: let a main server's Coolify build and run apps on a worker.
import { agentOn, coolifyOn } from "./api";
import type { CockpitSettings } from "../types";

export const POOL_STEPS = [
  "Read the main server's Coolify key",
  "Authorise it on the worker",
  "Find the worker's Tailscale address",
  "Register the worker in Coolify",
] as const;

export const isIpv4 = (a: string) => /^\d{1,3}(\.\d{1,3}){3}$/.test(a);

/**
 * Runs the four steps; `onStep(i)` fires as each one starts. Returns false
 * when the worker was already in the pool. Safe to repeat.
 */
export async function attachWorker(worker: CockpitSettings, main: CockpitSettings, onStep: (i: number) => void = () => {}): Promise<boolean> {
  onStep(0);
  const key = await agentOn(main.agent_url).workerKey();
  onStep(1);
  await agentOn(worker.agent_url).authorizeKey(key.public_key);
  onStep(2);
  const ts = await agentOn(worker.agent_url).tailscale();
  const ip = ts.self_node?.ips.find(isIpv4);
  if (!ip) throw new Error(`${worker.hostname} has no Tailscale IPv4 address — is Tailscale connected there?`);
  onStep(3);
  const c = coolifyOn(main.agent_url);
  if ((await c.servers()).some((x) => x.ip === ip)) return false;
  await c.createServer({
    name: worker.hostname,
    description: "Vercelify worker (Tailscale)",
    ip,
    port: 22,
    user: "root",
    private_key_uuid: key.private_key_uuid,
    is_build_server: false,
    instant_validate: true,
  });
  return true;
}

/** The install command for a new machine with the chosen options. */
export function installCommand(opts: { worker: boolean; hostname?: string; ai?: boolean }): string {
  const args: string[] = [];
  if (opts.worker) args.push("--worker");
  const h = (opts.hostname ?? "").trim().toLowerCase();
  if (h) args.push("--hostname", h);
  if (opts.ai) args.push("--ai");
  const base = "curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh | sudo bash";
  return args.length ? `${base} -s -- ${args.join(" ")}` : base;
}

/** Same rule as the installer's --hostname check. */
export const validHostname = (h: string) => h === "" || /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(h);
