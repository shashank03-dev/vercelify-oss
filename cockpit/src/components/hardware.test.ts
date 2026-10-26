import { describe, expect, it } from "vitest";
import type { HardwareInfo } from "../types";
import { bucket, hardwareFacts } from "./Hardware";
import { alertHealth } from "./ServerCare";

const pi: HardwareInfo = {
  profile: "pi4",
  model: "Raspberry Pi 4 Model B Rev 1.5",
  root_media: "sd_card",
  memory_cgroup: false,
  throttling: { raw: 0x50005, under_voltage_now: true, throttled_now: true, soft_temp_limit_now: false, under_voltage_since_boot: true, throttled_since_boot: true },
  pressure: { cpu_some_avg10: 1, memory_some_avg10: 22, io_some_avg10: 3 },
  oom_kills: 2,
  watchdog: true,
  defaults_pause_c: 78,
};

describe("hardware facts", () => {
  it("flags what hurts a Raspberry Pi", () => {
    const f = Object.fromEntries(hardwareFacts(pi).map((x) => [x.label, x]));
    expect(f["Machine"]!.value).toBe("Raspberry Pi 4 Model B Rev 1.5");
    expect(f["System disk"]!.health).toBe("warn");
    expect(f["Memory limits"]!.health).toBe("bad");
    expect(f["Power supply"]!.value).toBe("under-voltage now");
    expect(f["Power supply"]!.health).toBe("bad");
    expect(f["Memory pressure"]!.health).toBe("warn");
    expect(f["Out-of-memory kills"]!.value).toBe("2 since boot");
  });
  it("a healthy laptop is all OK", () => {
    const laptop: HardwareInfo = { ...pi, profile: "laptop", model: "", root_media: "ssd", memory_cgroup: true, throttling: null, pressure: null, oom_kills: 0 };
    const facts = hardwareFacts(laptop);
    expect(facts.find((x) => x.label === "Machine")!.value).toBe("Laptop");
    expect(facts.filter((x) => x.health === "bad")).toEqual([]);
  });
});

describe("history downsampling", () => {
  it("keeps spikes and gaps", () => {
    expect(bucket([1, 2, 3], 5)).toEqual([1, 2, 3]);
    expect(bucket([1, 9, 2, 2, null, null], 3)).toEqual([9, 2, null]);
  });
});

describe("alert health", () => {
  it("resolved is ok, otherwise by status", () => {
    const base = { id: 1, at: "", source: "x", title: "", detail: "", notified: null };
    expect(alertHealth({ ...base, status: "bad", resolved: true })).toBe("ok");
    expect(alertHealth({ ...base, status: "bad", resolved: false })).toBe("bad");
    expect(alertHealth({ ...base, status: "warn", resolved: false })).toBe("warn");
  });
});
