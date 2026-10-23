import type { Health } from "../types";

/**
 * Features an agent offers (`/v1/health` → `capabilities`). Servers can run
 * different versions; the UI asks before showing what an older agent lacks.
 */
export function hasCap(health: Health | undefined, cap: string): boolean {
  return !!health?.capabilities?.includes(cap);
}

/** "0.2.10" > "0.2.9": numeric, part by part. Unparseable parts count as 0. */
export function compareVersions(a: string, b: string): number {
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
