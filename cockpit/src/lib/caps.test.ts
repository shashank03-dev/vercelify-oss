import { describe, expect, it } from "vitest";
import { compareVersions, engineDownReason, hasCap, newestVersion } from "./caps";

describe("capabilities and versions", () => {
  it("reads capabilities, old agents have none", () => {
    const h = { ok: true, version: "0.2.0", hostname: "x", role: "main", pairing_open: false, api_version: 2, capabilities: ["alerts"] };
    expect(hasCap(h, "alerts")).toBe(true);
    expect(hasCap(h, "backup")).toBe(false);
    expect(hasCap(undefined, "alerts")).toBe(false);
    expect(hasCap({ ...h, capabilities: undefined as unknown as string[] }, "alerts")).toBe(false);
  });
  it("compares versions numerically", () => {
    expect(compareVersions("0.2.10", "0.2.9")).toBe(1);
    expect(compareVersions("0.1.0", "0.1.0")).toBe(0);
    expect(compareVersions("0.1", "0.1.1")).toBe(-1);
    expect(compareVersions("0.2.5+main.1a2b3c4", "0.2.5")).toBe(0);
    expect(compareVersions("0.2.5+main.9", "0.2.6")).toBe(-1);
    expect(newestVersion(["0.1.0", undefined, "0.2.0", "0.1.9"])).toBe("0.2.0");
    expect(newestVersion([])).toBeUndefined();
  });
  it("says why the engine isn't running", () => {
    expect(
      engineDownReason(
        "the platform engine failed to start: this server was replaced as the main server by pi5 (epoch 2): sudo vercelify demote, then join it as a worker",
      ),
    ).toBe("this server was replaced as the main server by pi5 (epoch 2): sudo vercelify demote, then join it as a worker");
    expect(engineDownReason("the platform engine is off on this server")).toBe("the platform engine is off on this server");
    expect(engineDownReason(undefined)).toBeUndefined();
  });
});
