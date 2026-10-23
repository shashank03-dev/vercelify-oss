// Storage-tier volume rules — mirror of crates/vercelify-core/src/volumes.rs.
// Both implementations are tested against
// crates/vercelify-core/tests/fixtures/volume-vectors.json.

import type { StorageTier } from "../types";

export const HDD_VOLUME_ROOT = "/mnt/storage/volumes";
export const SSD_VOLUME_ROOT = "/var/lib/docker/volumes";

export class VolumeError extends Error {
  constructor(readonly kind: "service" | "name") {
    super(
      kind === "service"
        ? "Service name: 1–63 lowercase letters, digits, '-', '_' or '.', starting with a letter or digit"
        : "Folder: letters, digits, '-', '_' or '.', separated by '/' (no '..')",
    );
  }
}

export function validateService(service: string): boolean {
  return /^[a-z0-9][a-z0-9_.-]{0,62}$/.test(service);
}

export function normalizeName(name: string | null | undefined): string | null {
  const raw = (name ?? "").trim();
  if (!raw) return null;
  const segs = raw.replace(/^\/+|\/+$/g, "").split("/").filter(Boolean);
  if (!segs.length || raw.length > 200) throw new VolumeError("name");
  for (const s of segs) {
    if (s === "." || s === ".." || !/^[A-Za-z0-9_.-]+$/.test(s)) throw new VolumeError("name");
  }
  return segs.join("/");
}

export function namedVolume(service: string, name: string | null): string {
  return name ? `${service}-${name.replace(/\//g, "-")}` : `${service}-data`;
}

export interface ResolvedVolume {
  /** Named volume (SSD) or absolute host path (HDD). */
  source: string;
  hostPath: string;
}

export function resolveVolume(tier: StorageTier, service: string, name?: string | null): ResolvedVolume {
  if (!validateService(service)) throw new VolumeError("service");
  const n = normalizeName(name);
  if (tier === "ssd") {
    const vol = namedVolume(service, n);
    return { source: vol, hostPath: `${SSD_VOLUME_ROOT}/${vol}/_data` };
  }
  const path = n ? `${HDD_VOLUME_ROOT}/${service}/${n}` : `${HDD_VOLUME_ROOT}/${service}`;
  return { source: path, hostPath: path };
}

export type ComposeStyle = "bind" | "named";

export interface SnippetInput {
  service: string;
  name?: string | null;
  tier: StorageTier;
  containerPath: string;
  /** HDD only: plain bind mount, or a named volume backed by the HDD path. */
  style?: ComposeStyle;
  image?: string;
}

export function validContainerPath(p: string): boolean {
  return /^\/[A-Za-z0-9_.\-/]*$/.test(p) && !p.split("/").includes("..");
}

/** docker-compose YAML for the chosen tier. */
export function composeSnippet(input: SnippetInput): string {
  const { source, hostPath } = resolveVolume(input.tier, input.service, input.name);
  if (!validContainerPath(input.containerPath)) throw new Error("Container path must be absolute, e.g. /var/lib/postgresql/data");
  const head = [`services:`, `  ${input.service}:`];
  if (input.image) head.push(`    image: ${input.image}`);
  head.push(`    volumes:`);

  if (input.tier === "hdd" && (input.style ?? "bind") === "bind") {
    return [...head, `      - ${source}:${input.containerPath}`, ""].join("\n");
  }
  const volName = input.tier === "ssd" ? source : namedVolume(input.service, normalizeName(input.name));
  const lines = [...head, `      - ${volName}:${input.containerPath}`, ``, `volumes:`];
  if (input.tier === "ssd") {
    lines.push(`  ${volName}: {}`);
  } else {
    lines.push(
      `  ${volName}:`,
      `    driver: local`,
      `    driver_opts:`,
      `      type: none`,
      `      o: bind`,
      `      device: ${hostPath}`,
    );
  }
  return [...lines, ""].join("\n");
}

export interface Preset {
  id: string;
  label: string;
  image: string;
  containerPath: string;
  name: string;
  recommended: StorageTier;
  why: string;
}

export const PRESETS: Preset[] = [
  { id: "postgres", label: "PostgreSQL", image: "postgres:17-alpine", containerPath: "/var/lib/postgresql/data", name: "data", recommended: "hdd", why: "Large, long-lived data" },
  { id: "mysql", label: "MySQL / MariaDB", image: "mariadb:11", containerPath: "/var/lib/mysql", name: "data", recommended: "hdd", why: "Large, long-lived data" },
  { id: "mongodb", label: "MongoDB", image: "mongo:7", containerPath: "/data/db", name: "data", recommended: "hdd", why: "Large, long-lived data" },
  { id: "minio", label: "MinIO (S3)", image: "minio/minio:latest", containerPath: "/data", name: "objects", recommended: "hdd", why: "Bulk object storage" },
  { id: "valkey", label: "Valkey / Redis", image: "valkey/valkey:8-alpine", containerPath: "/data", name: "data", recommended: "ssd", why: "Small and latency-sensitive" },
  { id: "cache", label: "Build / image cache", image: "", containerPath: "/app/.next/cache", name: "cache", recommended: "ssd", why: "Hot, disposable data" },
  { id: "uploads", label: "User uploads", image: "", containerPath: "/app/uploads", name: "uploads", recommended: "hdd", why: "Grows without bound" },
];
