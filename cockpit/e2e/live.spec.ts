import { expect, test, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import http from "node:http";

// Live end-to-end: started by e2e/run-live.sh with a real agent running
// Vercelify's engine on this machine's Docker. Each test drives the
// production UI like a user would.

const AGENT_URL = process.env.E2E_AGENT_URL!;
const AGENT_TOKEN = process.env.E2E_AGENT_TOKEN!;
const HDD = process.env.E2E_HDD!;
const IMAGE = process.env.E2E_IMAGE!;
const PROXY_HTTP = process.env.E2E_PROXY_HTTP!;
const WILDCARD = "e2e.localhost";

function pairingCode(): string {
  // The browser preview reaches the agent through its /__agent proxy, so the
  // address in the code is only shown.
  const json = JSON.stringify({ agent_url: "http://10.77.0.1:7420", agent_token: AGENT_TOKEN, hostname: "vercelify-host", role: "main" });
  return "vfy1." + Buffer.from(json).toString("base64url");
}

async function platformApi<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
  const r = await fetch(`${AGENT_URL}/v1/platform${path}`, {
    method,
    headers: { Authorization: `Bearer ${AGENT_TOKEN}`, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await r.text();
  if (!r.ok) throw new Error(`${method} ${path}: HTTP ${r.status} ${text}`);
  return (text ? JSON.parse(text) : null) as T;
}

/** An app's page through Vercelify's own proxy, by its host name (fetch can't set Host). */
function sitePage(name: string): Promise<string> {
  return new Promise((resolve) => {
    const req = http.get({ host: "127.0.0.1", port: Number(PROXY_HTTP), path: "/", headers: { Host: `${name}.${WILDCARD}` }, timeout: 5_000 }, (res) => {
      let body = "";
      res.on("data", (c) => (body += c));
      res.on("end", () => resolve(res.statusCode === 200 ? body : ""));
    });
    req.on("error", () => resolve(""));
    req.on("timeout", () => req.destroy());
  });
}

function docker(...args: string[]): string {
  try {
    return execFileSync("docker", args).toString().trim();
  } catch {
    return "";
  }
}

/** A command in a Redis/Valkey database's container, with its password. */
function redis(db: string, cmd: string): string {
  return docker("exec", `vfy-db-${db}`, "sh", "-c", `REDISCLI_AUTH="$VFY_PASSWORD" "$(command -v valkey-cli || echo redis-cli)" ${cmd}`);
}

async function shot(page: Page, name: string) {
  await page.waitForTimeout(400); // let springs settle
  await page.screenshot({ path: `../test-results/screens/${name}.png`, fullPage: false });
}

async function paired(page: Page) {
  await page.goto("/");
  await page.evaluate(() => localStorage.clear());
  await page.goto("/");
  await page.getByTestId("pairing-code").fill(pairingCode());
  await page.getByTestId("pair-submit").click();
  await expect(page.getByTestId("nav-apps")).toBeVisible();
}

test.beforeAll(async () => {
  // Apps get <name>.e2e.localhost, served over plain HTTP by the proxy. (Each
  // run starts with a fresh engine database: run-live.sh.)
  await platformApi("PUT", "/settings/domain", { wildcard: WILDCARD, https: false, edge_tls: false });
});

test("onboarding rejects a bad code, then pairs", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => localStorage.clear());
  await page.goto("/");
  await expect(page.getByText("Welcome to")).toBeVisible();
  await shot(page, "01-onboarding");
  await page.getByTestId("pairing-code").fill("vfy1.not-a-real-code!!");
  await page.getByTestId("pair-submit").click();
  await expect(page.getByRole("alert")).toContainText("base64");
  await page.getByTestId("pairing-code").fill(pairingCode());
  await page.getByTestId("pair-submit").click();
  await expect(page.getByTestId("host-status")).toContainText("via 10.77.0.1");
  await expect(page.getByTestId("platform-apps")).toBeVisible();
});

test("apps: create from an image, live log, served by the proxy, redeploy, roll back, runtime logs", async ({ page }) => {
  await paired(page);
  await page.getByTestId("new-app").click();
  await page.getByTestId("kind-image").click();
  await page.getByTestId("new-image").fill(IMAGE);
  await page.getByTestId("new-name").fill("e2e-whoami");
  await page.getByTestId("new-port").fill("80");
  await page.getByTestId("create-app").click();
  await expect(page.getByTestId("app-detail")).toContainText("e2e-whoami");
  // "Create and deploy" starts the first deployment
  await expect(page.getByTestId("deployment-1")).toContainText("Live", { timeout: 150_000 });
  await expect(page.getByTestId("deploy-log")).toContainText("healthy: http://e2e-whoami.e2e.localhost");
  await shot(page, "02-deployed");
  await expect.poll(() => sitePage("e2e-whoami"), { timeout: 60_000 }).toContain("Hostname:");

  // a second deployment, then back to the first
  await page.getByTestId("deploy-now").click();
  await expect(page.getByTestId("deployment-2")).toContainText("Live", { timeout: 150_000 });
  const second = docker("ps", "--filter", "label=vercelify.app-name=e2e-whoami", "--format", "{{.Names}}");
  await page.getByTestId("rollback-1").click();
  await expect(page.getByTestId("deployment-3")).toContainText("Live", { timeout: 150_000 });
  await expect.poll(() => docker("ps", "--filter", "label=vercelify.app-name=e2e-whoami", "--format", "{{.Names}}"), { timeout: 60_000 }).not.toBe(second);
  await expect.poll(() => sitePage("e2e-whoami"), { timeout: 60_000 }).toContain("Hostname:");
  await shot(page, "03-rolled-back");

  // what the app itself prints
  await page.getByTestId("tab-logs").click();
  await expect(page.getByTestId("runtime-log")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("status-e2e-whoami")).toContainText("Live");
});

test("databases: one-click Redis, its URL, a backup (or why not)", async ({ page }) => {
  await paired(page);
  await page.getByTestId("new-db").click();
  await page.getByTestId("db-kind").selectOption("redis");
  await page.getByTestId("db-name").fill("e2e-cache");
  await page.getByTestId("db-create").click();
  await expect(page.getByTestId("db-detail")).toContainText("e2e-cache");
  await expect(page.getByTestId("db-url")).toContainText("redis://");
  await expect.poll(() => redis("e2e-cache", "ping"), { timeout: 120_000 }).toContain("PONG");
  await page.getByTestId("db-backup").click();
  // Backups go to the server's restic repository: without one, it says how to set it up.
  if (process.env.E2E_RESTIC) await expect(page.getByTestId("db-backups")).toContainText("just now", { timeout: 60_000 });
  else await expect(page.getByText("backups aren't set up on this server: sudo vercelify backup setup")).toBeVisible();
  await shot(page, "04-database");
});

test("storage: tiers, assistant, move a live database between SSD and HDD", async ({ page }) => {
  // its own database, with a key to find again after the moves
  await platformApi("POST", "/databases", { name: "e2e-store", kind: "redis" });
  await expect.poll(() => redis("e2e-store", "ping"), { timeout: 120_000 }).toContain("PONG");
  redis("e2e-store", "set e2e survives");
  redis("e2e-store", "save");
  await paired(page);
  await page.getByTestId("nav-storage").click();
  await expect(page.getByTestId("tier-card-ssd")).toContainText("free");

  // Volume assistant
  await page.getByTestId("service-name").fill("e2e-postgres");
  await page.getByTestId("tier-hdd").click();
  await expect(page.getByTestId("compose-snippet")).toContainText("/mnt/storage/volumes/e2e-postgres/data:/var/lib/postgresql/data");
  await page.getByTestId("tier-ssd").click();
  await expect(page.getByTestId("compose-snippet")).toContainText("e2e-postgres-data: {}");
  await page.getByTestId("tier-hdd").click();
  await page.getByTestId("create-volume").click();
  await expect(page.getByText(/Created .*e2e-postgres\/data|already exists/)).toBeVisible();
  expect(existsSync(`${HDD}/volumes/e2e-postgres/data`)).toBe(true);
  await page.getByTestId("service-name").fill("Bad Name");
  await expect(page.getByRole("alert")).toContainText("Service name");
  await shot(page, "05-storage");

  // Move the database's data (whichever tier it is on) and back; it survives.
  const row = page.getByTestId("pvolume-e2e-store-data");
  await expect(row).toBeVisible();
  for (let i = 0; i < 2; i++) {
    const button = row.getByRole("button", { name: /Move to/ });
    const target = (await button.textContent())!.includes("HDD") ? "HDD" : "SSD";
    await button.click();
    await page.getByTestId("pmove-run").click();
    await expect(page.getByText(`is on the ${target} now`)).toBeVisible({ timeout: 150_000 });
    await expect(row.getByText(target, { exact: true })).toBeVisible();
    await shot(page, `06-move-${target.toLowerCase()}`);
  }
  await expect
    .poll(() => redis("e2e-store", "get e2e"), { timeout: 90_000 })
    .toBe("survives");
});

test("node health: resources, power, containers, logs, services", async ({ page }) => {
  await paired(page);
  await page.getByTestId("nav-health").click();
  await expect(page.getByTestId("resources")).toContainText("CPU");
  await expect(page.getByTestId("power-source")).toBeVisible();
  const proxy = page.getByTestId("container-vercelify-proxy");
  await expect(proxy).toBeVisible();
  await expect(proxy).toContainText("%");
  await shot(page, "07-health");
  await proxy.getByRole("button", { name: "Logs" }).click();
  await expect(page.getByTestId("container-logs")).not.toBeEmpty();
  await expect(page.getByTestId("container-logs")).not.toHaveText("Loading…");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("services")).toContainText("docker");
});

test("screencast: browser explains desktop-only capture before starting a stream", async ({ page }) => {
  await paired(page);
  await page.getByTestId("nav-screencast").click();
  await expect(page.getByTestId("extend-card")).toContainText("mpv on the host");
  await page.getByTestId("display-advanced").click();
  await page.getByTestId("source-test").click();
  await page.getByTestId("start-extend").click();
  // In the browser preview there is no local ffmpeg (and a host off the mesh has
  // no address to stream to): the UI must explain the requirement.
  await expect(page.getByText(/Available in the desktop app only|has no private address yet/)).toBeVisible();
  await expect(page.getByTestId("start-extend")).toBeVisible();
  await shot(page, "08-screencast");
});

test("settings: unpair returns to onboarding", async ({ page }) => {
  await paired(page);
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("settings-computer").click();
  await page.getByTestId("unpair").click();
  await expect(page.getByTestId("pairing-code")).toBeVisible();
});
