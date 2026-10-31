import type { Health } from "../types";

/**
 * Features an agent offers (`/v1/health` → `capabilities`). Servers can run
 * different versions; the UI asks before showing what an older agent lacks.
 */
export function hasCap(health: Health | undefined, cap: string): boolean {
  return !!health?.capabilities?.includes(cap);
}

/**
 * "0.2.10" > "0.2.9": numeric, part by part. Unparseable parts count as 0.
 * Build metadata (`0.2.5+main.1a2b3c4`, a build from a branch) is ignored.
 */
export function compareVersions(a: string, b: string): number {
  [a, b] = [a.split("+")[0] ?? a, b.split("+")[0] ?? b];
  const pa = a.split(/[.-]/).map((x) => Number.parseInt(x, 10) || 0);
  const pb = b.split(/[.-]/).map((x) => Number.parseInt(x, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d) return d < 0 ? -1 : 1;
  }
  return 0;
}

/** Newest version among `versions` (undefined entries ignored). */
export function newestVersion(versions: (string | undefined)[]): string | undefined {
  return versions.filter((v): v is string => !!v).sort(compareVersions).at(-1);
}

/**
 * Why a server without the `platform` capability has no engine, from the
 * agent's answer to a platform request: the engine didn't start (a promoted
 * standby replaced the server, its database can't be opened…) or is off.
 */
export function engineDownReason(platformError: string | undefined): string | undefined {
  const prefix = "the platform engine failed to start: ";
  return platformError?.startsWith(prefix) ? platformError.slice(prefix.length) : platformError;
}
