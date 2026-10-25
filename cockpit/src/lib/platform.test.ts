import { describe, expect, it } from "vitest";
import type { App } from "../types";
import { isWorker } from "./api";
import { appState, fromBase64, isPlatform, listedApp, parseDotenv, parseLines, parseLogLines, slug, toBase64, validName, validRepo } from "./platform";

const app = (over: Partial<App> = {}): App => ({
  id: "app_1",
  name: "shop",
  project_id: "p",
  environment_id: "e",
  environment: "production",
  kind: "git",
  source: { repository: "https://github.com/me/shop", branch: "main", git_source_id: null, image: null, compose_file: null, compose_content: null, pinned_sha: null },
  build: { pack: "auto", dockerfile: null, context: null, install_command: null, build_command: null, output_directory: null, watch_paths: [] },
  runtime: { port: null, command: null, replicas: 1, memory_mb: null, cpus: null, health: null, health_timeout_secs: 120, drain_secs: 10, service: null, failover: false },
  previews: { enabled: true, max: 3 },
  node_id: null,
  desired: "running",
  current_deployment_id: "dep_1",
  last_status: "healthy",
  domains: [],
  url: "https://shop.example.com",
  created_at: "",
  updated_at: "",
  ...over,
});

describe("platform helpers", () => {
  it("tells platform mains from workers and Coolify mains", () => {
    const base = { agent_url: "http://h:7420", agent_token: "t", coolify_token: null, hostname: "h" };
    expect(isPlatform({ ...base, coolify_url: "", role: "main" })).toBe(true);
    expect(isWorker({ ...base, coolify_url: "", role: "main" })).toBe(false);
    expect(isPlatform({ ...base, coolify_url: "http://h:8000", role: "main" })).toBe(false);
    expect(isPlatform({ ...base, coolify_url: "", role: "worker" })).toBe(false);
    // pairings from before roles: no Coolify means a worker
    expect(isWorker({ ...base, coolify_url: "" })).toBe(true);
  });

  it("derives an app's state from what it should do and its last deployment", () => {
    expect(appState(app())).toEqual({ health: "ok", label: "Live" });
    expect(appState(app({ last_status: "building" })).label).toBe("Building");
    expect(appState(app({ last_status: "failed" })).health).toBe("warn");
    expect(appState(app({ current_deployment_id: null, last_status: "failed" })).health).toBe("bad");
    expect(appState(app({ current_deployment_id: null, last_status: null })).label).toBe("Not deployed");
    expect(appState(app({ desired: "stopped" })).label).toBe("Stopped");
    expect(appState(app({ desired: "sleeping" })).label).toBe("Asleep");
  });

  it("maps apps to the list shape the chrome uses", () => {
    const l = listedApp(app({ last_status: "failed", current_deployment_id: null }));
    expect(l).toMatchObject({ uuid: "app_1", name: "shop", fqdn: "https://shop.example.com", status: "exited:unhealthy" });
    expect(listedApp(app()).status).toBe("running:healthy");
    expect(listedApp(app({ kind: "image" })).build_pack).toBe("dockerimage");
  });

  it("validates names and repositories like the agent", () => {
    expect(slug("My Blog!")).toBe("my-blog");
    expect(validName("my-blog")).toBe(true);
    expect(validName("-x")).toBe(false);
    expect(validName("a".repeat(41))).toBe(false);
    expect(validRepo("https://github.com/me/app")).toBe(true);
    expect(validRepo("git@github.com:me/app.git")).toBe(true);
    expect(validRepo("github.com/me/app")).toBe(false);
  });

  it("parses .env files and log lines", () => {
    expect(parseDotenv('# c\nexport A=1\nB="x\\ny"\nC=\'q\'\nD=v # note\nbad line\n1X=2')).toEqual([
      { key: "A", value: "1" },
      { key: "B", value: "x\ny" },
      { key: "C", value: "q" },
      { key: "D", value: "v" },
    ]);
    const line = { seq: 1, at: "t", stream: "info", text: "hi" };
    expect(parseLogLines(`${JSON.stringify(line)}\n{broken\n`)).toEqual([line]);
    expect(parseLogLines([line])).toEqual([line]);
    expect(parseLogLines(null)).toEqual([]);
  });

  it("carries terminal bytes as base64, UTF-8 included", () => {
    expect(toBase64("ls -la\r")).toBe(btoa("ls -la\r"));
    const b = toBase64("héllo ✓");
    expect(new TextDecoder().decode(fromBase64(b))).toBe("héllo ✓");
    // A character split across two chunks still decodes once joined.
    const bytes = fromBase64(b);
    const d = new TextDecoder();
    expect(d.decode(bytes.slice(0, 2), { stream: true }) + d.decode(bytes.slice(2))).toBe("héllo ✓");
    expect(parseLines<{ text: string }>('{"text":"a"}\nnot json\n{"text":"b"}\n').map((l) => l.text)).toEqual(["a", "b"]);
  });
});
