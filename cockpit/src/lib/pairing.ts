// Mirror of crates/vercelify-core/src/pairing.rs (browser mode only; the
// desktop app decodes in Rust).

import type { Pairing } from "../types/generated/Pairing";

export const PAIRING_PREFIX = "vfy1.";

export function decodePairing(code: string): Pairing {
  const trimmed = code.trim();
  if (!trimmed.startsWith(PAIRING_PREFIX)) throw new Error(`pairing code must start with '${PAIRING_PREFIX}'`);
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
  let p: Pairing;
  try {
    p = JSON.parse(json) as Pairing;
  } catch (e) {
    throw new Error(`pairing code payload is invalid: ${(e as Error).message}`);
  }
  if (!/^https?:\/\//.test(p.agent_url ?? "")) throw new Error("pairing code payload is invalid: agent_url must be http(s)");
  if ((p.agent_token ?? "").length < 16) throw new Error("pairing code payload is invalid: agent_token too short");
  return p;
}

export function encodePairing(p: Pairing): string {
  const bytes = new TextEncoder().encode(JSON.stringify(p));
  const b64 = btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  return PAIRING_PREFIX + b64;
}
