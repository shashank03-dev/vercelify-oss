import type { MeshPeer } from "../types";

/** How a stream to the server travels, for the Second screen page. */
export type StreamPath = {
  /** `local`: this network; `internet`: out and back in (a hotspot that keeps
   * devices apart does this); `relay`: through the server's relay. */
  kind: "local" | "internet" | "relay" | "none" | "unknown";
  name?: string;
  /** Round trip in ms, when the mesh measured it. */
  rtt?: number;
};

/** An address on this network: private IPv4, link-local, or IPv6 ULA. */
function isPrivate(endpoint: string): boolean {
  const host = endpoint.startsWith("[") ? endpoint.slice(1, endpoint.indexOf("]")) : endpoint.replace(/:\d+$/, "");
  if (host.includes(":")) return /^f[cd]/i.test(host) || /^fe80:/i.test(host);
  const [a, b] = host.split(".").map(Number);
  return a === 10 || (a === 172 && b! >= 16 && b! <= 31) || (a === 192 && b === 168) || (a === 169 && b === 254);
}

/** The path to the server at mesh address `host`, from this computer's peers. */
export function streamPath(peers: MeshPeer[], host: string): StreamPath {
  const p = peers.find((x) => x.addresses.includes(host));
  if (!p) return { kind: "unknown" };
  const rtt = p.rtt_ms === undefined ? undefined : Math.round(p.rtt_ms);
  const kind =
    p.path === "lan" ? "local"
    : p.path === "direct" ? (p.endpoint && isPrivate(p.endpoint) ? "local" : "internet")
    : p.path === "relay" ? "relay"
    : "none";
  return rtt === undefined ? { kind, name: p.name } : { kind, name: p.name, rtt };
}
