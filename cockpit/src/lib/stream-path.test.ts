import { streamPath } from "./stream-path";
import type { MeshPeer } from "../types";

const peer = (path: string, endpoint?: string, rtt_ms?: number): MeshPeer => ({ name: "laptop", addresses: ["10.77.0.1"], path, endpoint, rtt_ms });

describe("streamPath", () => {
  it("is local on the LAN, or direct to a private address", () => {
    expect(streamPath([peer("lan", "192.168.1.20:51820", 2)], "10.77.0.1").kind).toBe("local");
    expect(streamPath([peer("direct", "192.168.191.207:51820", 5)], "10.77.0.1").kind).toBe("local");
    expect(streamPath([peer("direct", "[fd12::5]:51820")], "10.77.0.1").kind).toBe("local");
  });

  it("is the internet when direct to a public address, as through a hotspot that keeps devices apart", () => {
    const p = streamPath([peer("direct", "49.43.246.18:51820", 212.5)], "10.77.0.1");
    expect(p).toEqual({ kind: "internet", name: "laptop", rtt: 213 });
  });

  it("says relay and no path as they are", () => {
    expect(streamPath([peer("relay")], "10.77.0.1").kind).toBe("relay");
    expect(streamPath([peer("none")], "10.77.0.1").kind).toBe("none");
  });

  it("knows nothing about a server that isn't reached over the mesh", () => {
    expect(streamPath([peer("relay")], "192.168.1.20").kind).toBe("unknown");
    expect(streamPath([], "10.77.0.1").kind).toBe("unknown");
  });
});
