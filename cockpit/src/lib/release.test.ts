import { afterEach, describe, expect, it, vi } from "vitest";

const call = vi.fn();
vi.mock("./transport", () => ({ call: (...a: unknown[]) => call(...a) }));

const { appBehind, forgetLatestRelease, latestRelease } = await import("./release");

afterEach(() => {
  call.mockReset();
  forgetLatestRelease();
});

describe("latest release", () => {
  it("asks GitHub once for the whole app, and again after a few hours", async () => {
    call.mockResolvedValue({ version: "0.3.0", url: "https://x", current: "0.2.5" });
    await latestRelease(0);
    await latestRelease(1000);
    expect(call).toHaveBeenCalledTimes(1);
    await latestRelease(7 * 3600 * 1000);
    expect(call).toHaveBeenCalledTimes(2);
  });

  it("offline or in the browser there is simply no notice", async () => {
    call.mockRejectedValue(new Error("Available in the desktop app only"));
    expect(await latestRelease(0)).toBeUndefined();
  });

  it("only an older app is behind", () => {
    expect(appBehind({ version: "0.3.0", url: "", current: "0.2.5" })).toBe(true);
    expect(appBehind({ version: "0.2.5", url: "", current: "0.2.5" })).toBe(false);
    expect(appBehind({ version: "0.2.5", url: "", current: "0.10.0" })).toBe(false);
    expect(appBehind(undefined)).toBe(false);
  });
});
