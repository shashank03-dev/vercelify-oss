import { ago, bytes, daysUntil, duration, mediaLabel, primaryUrl, profileLabel, shortSha, statusHealth, statusLabel } from "./format";

describe("format", () => {
  it("bytes", () => {
    expect(bytes(512)).toBe("512 B");
    expect(bytes(1536)).toBe("1.5 KB");
    expect(bytes(256060514304)).toBe("238 GB");
    expect(bytes(null)).toBe("—");
  });

  it("duration", () => {
    expect(duration(42)).toBe("42s");
    expect(duration(3 * 60)).toBe("3m");
    expect(duration(5 * 3600 + 120)).toBe("5h 2m");
    expect(duration(3 * 86400 + 7200)).toBe("3d 2h");
  });

  it("ago understands ISO and Coolify timestamps", () => {
    const now = Date.parse("2026-10-18T12:00:00Z");
    expect(ago("2026-10-18T11:58:00Z", now)).toBe("2m ago");
    expect(ago("2026-10-18 09:00:00", now)).toBe("3h ago");
    expect(ago(null, now)).toBe("never");
  });

  it("status mapping", () => {
    expect(statusHealth("running:healthy")).toBe("ok");
    expect(statusHealth("running:unhealthy")).toBe("warn");
    expect(statusHealth("exited:unhealthy")).toBe("bad");
    expect(statusHealth("restarting")).toBe("warn");
    expect(statusHealth("finished")).toBe("ok");
    expect(statusLabel("running:unknown")).toBe("running");
    expect(statusLabel("running:healthy")).toBe("running · healthy");
  });

  it("misc", () => {
    expect(primaryUrl("http://a.example.com, https://b.example.com")).toBe("http://a.example.com");
    expect(primaryUrl(null)).toBeNull();
    expect(shortSha("a9ebabfe69246fc375b0e09d056e013866f0faeb")).toBe("a9ebabf");
    expect(shortSha("HEAD")).toBe("HEAD");
  });
});


describe("hardware labels", () => {
  it("names media and profiles", () => {
    expect(mediaLabel("sd_card")).toBe("SD card");
    expect(mediaLabel("ssd")).toBe("SSD");
    expect(mediaLabel("unknown")).toBe("");
    expect(profileLabel("pi5")).toBe("Raspberry Pi 5");
    expect(profileLabel(undefined)).toBe("Unknown machine");
  });
  it("counts days to an expiry", () => {
    const now = Date.parse("2026-10-20T00:00:00Z");
    expect(daysUntil("2026-10-30T12:00:00Z", now)).toBe(10);
    expect(daysUntil("2026-10-19T12:00:00Z", now)).toBe(-1);
    expect(daysUntil(null, now)).toBeNull();
    expect(daysUntil("garbage", now)).toBeNull();
  });
});
