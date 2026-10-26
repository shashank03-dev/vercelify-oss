import { describe, expect, it } from "vitest";
import { backoff, mergeJob } from "../lib/live";
import { alertText, shouldNotify } from "../lib/notify";
import type { Alert, Job } from "../types";
import { jobHealth, jobLabel } from "./Activity";

const job = (id: string, patch: Partial<Job> = {}): Job => ({
  id,
  kind: "backup",
  title: "Backup",
  status: "running",
  started_at: "2026-10-20T10:00:00Z",
  finished_at: null,
  detail: null,
  progress: null,
  target: null,
  ...patch,
});

describe("live jobs", () => {
  it("updates a job in place and puts new ones first", () => {
    let list = mergeJob([], job("a"));
    list = mergeJob(list, job("b"));
    expect(list.map((j) => j.id)).toEqual(["b", "a"]);
    list = mergeJob(list, job("a", { status: "ok", progress: 100 }));
    expect(list.map((j) => [j.id, j.status])).toEqual([
      ["b", "running"],
      ["a", "ok"],
    ]);
  });

  it("keeps the list bounded", () => {
    let list: Job[] = [];
    for (let i = 0; i < 80; i++) list = mergeJob(list, job(`j${i}`));
    expect(list).toHaveLength(60);
    expect(list[0]!.id).toBe("j79");
  });

  it("labels jobs as dot + word", () => {
    expect(jobLabel(job("a", { progress: 41.6 }))).toBe("42%");
    expect(jobLabel(job("a"))).toBe("running");
    expect([jobHealth(job("a", { status: "failed" })), jobLabel(job("a", { status: "failed" }))]).toEqual(["bad", "failed"]);
    expect(jobHealth(job("a", { status: "ok" }))).toBe("ok");
  });

  it("reconnects with a capped backoff", () => {
    expect([1, 2, 3, 6, 10].map(backoff)).toEqual([2, 4, 8, 60, 60]);
  });
});

describe("native notifications", () => {
  const alert = (patch: Partial<Alert>): Alert => ({
    id: 1,
    at: "2026-10-20T10:00:00Z",
    source: "disk",
    status: "bad",
    title: "System disk: needs attention",
    detail: "96% used",
    resolved: false,
    notified: null,
    ...patch,
  });
  it("only for problems and recoveries", () => {
    expect(shouldNotify(alert({}))).toBe(true);
    expect(shouldNotify(alert({ status: "ok", resolved: true }))).toBe(true);
    expect(shouldNotify(alert({ status: "ok", source: "repair:1", resolved: true }))).toBe(false);
    expect(shouldNotify(alert({ source: "test:1" }))).toBe(false);
    expect(alertText("pi5", alert({}))).toEqual({ title: "pi5: System disk: needs attention", body: "96% used" });
  });
});

describe("menu-bar item", () => {
  it("counts open problems once per source", async () => {
    const { openProblems, trayText } = await import("../lib/live");
    const a = (id: number, source: string, status: Alert["status"], resolved = false) =>
      ({ id, at: "", source, status, title: "", detail: "", resolved, notified: null }) as Alert;
    // Newest first: disk recovered, power still warning, an older power alert is ignored.
    const list = [a(3, "disk", "ok", true), a(2, "power", "warn"), a(1, "disk", "bad"), a(0, "power", "bad")];
    expect(openProblems(list)).toBe(1);
    expect(trayText("pi5", true, 1)).toBe("pi5 · 1 needs attention");
    expect(trayText("pi5", true, 2)).toBe("pi5 · 2 need attention");
    expect(trayText("pi5", true, 0)).toBe("pi5 · all good");
    expect(trayText("pi5", false, 3)).toBe("pi5 · not reachable");
  });
});
