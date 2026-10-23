import { cronState, projectFileName, summaryLine, untilLabel } from "./project";
import type { CronInfo, ProjectSummary } from "../types";

const cron = (c: Partial<CronInfo>): CronInfo => ({
  path: "/api/cron",
  schedule: "0 5 * * *",
  next_run_at: null,
  last_run_at: null,
  last_status: null,
  last_error: null,
  last_duration_ms: null,
  running: false,
  ...c,
});

describe("projectFileName", () => {
  it("maps downloads back to the names the agent reads", () => {
    expect(projectFileName("vercel.json")).toBe("vercel.json");
    expect(projectFileName("vercel (1).json")).toBe("vercel.json");
    expect(projectFileName("Vercelify.JSON")).toBe("vercelify.json");
    expect(projectFileName("now.json")).toBe("vercel.json");
    expect(projectFileName("package.json")).toBeNull();
    expect(projectFileName("vercel.yaml")).toBeNull();
  });
});

describe("cronState", () => {
  const now = Date.parse("2026-10-22T10:00:00Z");
  it("shows the last run as dot + label", () => {
    expect(cronState(cron({}), now)).toEqual({ health: "idle", label: "not run yet" });
    expect(cronState(cron({ running: true, last_run_at: "2026-10-22T09:00:00Z" }), now).health).toBe("warn");
    expect(cronState(cron({ last_run_at: "2026-10-22T09:58:00Z", last_status: 200 }), now)).toEqual({ health: "ok", label: "HTTP 200 · 2m ago" });
    expect(cronState(cron({ last_run_at: "2026-10-22T09:58:00Z", last_status: 500, last_error: "answered HTTP 500" }), now).health).toBe("bad");
    expect(cronState(cron({ last_run_at: "2026-10-22T09:58:00Z", last_error: "no answer" }), now).label).toContain("no answer");
  });
});

describe("summaryLine", () => {
  const s = (p: Partial<ProjectSummary>): ProjectSummary => ({
    redirects: 0,
    rewrites: 0,
    headers: 0,
    firewall_rules: 0,
    clean_urls: false,
    trailing_slash: null,
    crons: [],
    warnings: [],
    ...p,
  });
  it("counts what is there", () => {
    expect(summaryLine(s({}))).toBe("no rules");
    expect(summaryLine(s({ redirects: 3, rewrites: 1, clean_urls: true, trailing_slash: false, crons: [cron({})] }))).toBe(
      "3 redirects · 1 rewrite · 1 cron job · clean URLs · no trailing slash",
    );
  });
});

describe("untilLabel", () => {
  const now = Date.parse("2026-10-22T10:00:00Z");
  it("counts down to the next run", () => {
    expect(untilLabel(null, now)).toBe("");
    expect(untilLabel("2026-10-22T10:00:30Z", now)).toBe("in under a minute");
    expect(untilLabel("2026-10-22T10:12:00Z", now)).toBe("in 12m");
    expect(untilLabel("2026-10-22T15:30:00Z", now)).toBe("in 5h");
    expect(untilLabel("2026-10-25T10:00:00Z", now)).toBe("in 3d");
  });
});
