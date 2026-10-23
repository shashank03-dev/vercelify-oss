import { CATALOG, fit, memoryNeeded, tokensPerSecond } from "./models";

const GB = 1024 ** 3;

describe("model catalog", () => {
  it("has unique, valid Ollama names", () => {
    const names = CATALOG.map((m) => m.name);
    expect(new Set(names).size).toBe(names.length);
    for (const n of names) expect(n).toMatch(/^[a-z0-9][a-z0-9._:/-]*$/);
  });

  it("judges fit against free and total memory", () => {
    // 8 GB laptop with 3 GB free
    expect(fit(2 * GB, 3 * GB, 8 * GB)).toBe("tight");
    expect(fit(0.8 * GB, 3 * GB, 8 * GB)).toBe("ok");
    expect(fit(4.9 * GB, 3 * GB, 8 * GB)).toBe("too-big");
    // unknown memory: don't block
    expect(fit(4.9 * GB, 0, 0)).toBe("ok");
    expect(memoryNeeded(GB)).toBeGreaterThan(GB);
  });

  it("computes tokens per second from Ollama's final chunk", () => {
    expect(tokensPerSecond({ eval_count: 50, eval_duration: 5e9 })).toBe(10);
    expect(tokensPerSecond({})).toBeNull();
  });
});
