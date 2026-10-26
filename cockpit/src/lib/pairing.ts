// Mirror of crates/vercelify-core/src/pairing.rs (browser mode only; the
// desktop app decodes in Rust).

import type { Pairing } from "../types/generated/Pairing";

export const PAIRING_PREFIX = "vfy1.";
/** A server on Vercelify's own mesh: the code carries a device join token. */
export const PAIRING_PREFIX_MESH = "vfy2.";

export function decodePairing(code: string): Pairing {
  const trimmed = code.trim();
  const mesh = trimmed.startsWith(PAIRING_PREFIX_MESH);
  if (!mesh && !trimmed.startsWith(PAIRING_PREFIX))
    throw new Error(`pairing code must start with '${PAIRING_PREFIX}' or '${PAIRING_PREFIX_MESH}'`);
  const body = trimmed.slice(PAIRING_PREFIX.length).replace(/[\s=]/g, "");
  if (!/^[A-Za-z0-9_-]+$/.test(body)) throw new Error("pairing code is not valid base64");
  let json: string;
  try {
    const b64 = body.replace(/-/g, "+").replace(/_/g, "/");
    const bin = atob(b64 + "===".slice((b64.length + 3) % 4));
    json = new TextDecoder().decode(Uint8Array.from(bin, (c) => c.charCodeAt(0)));
  } catch {
    throw new Error("pairing code is not valid base64");
  }
  let raw: Pairing;
  try {
    raw = JSON.parse(json) as Pairing;
  } catch (e) {
    throw new Error(`pairing code payload is invalid: ${(e as Error).message}`);
  }
  // Only the fields the Rust decoder knows (older codes also carried Coolify's URL and token).
  const p: Pairing = { agent_url: raw.agent_url, agent_token: raw.agent_token, hostname: raw.hostname };
  if (raw.role) p.role = raw.role;
  if (raw.mesh_join) p.mesh_join = raw.mesh_join;
  if (!/^https?:\/\//.test(p.agent_url ?? "")) throw new Error("pairing code payload is invalid: agent_url must be http(s)");
  if ((p.agent_token ?? "").length < 16) throw new Error("pairing code payload is invalid: agent_token too short");
  if (mesh !== Boolean(p.mesh_join)) throw new Error("pairing code payload is invalid: a vfy2. code carries a mesh join token, a vfy1. code doesn't");
  if (p.mesh_join && !p.mesh_join.startsWith("vfyj1.")) throw new Error("pairing code payload is invalid: mesh_join must be a vfyj1. token");
  return p;
}

export function encodePairing(p: Pairing): string {
  const bytes = new TextEncoder().encode(JSON.stringify(p));
  const b64 = btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  return (p.mesh_join ? PAIRING_PREFIX_MESH : PAIRING_PREFIX) + b64;
}
