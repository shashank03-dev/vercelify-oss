import { expect, test, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";

// Live end-to-end: started by e2e/run-live.sh with a real agent and a real
// Coolify. Each test drives the production UI like a user would.

const AGENT_TOKEN = process.env.E2E_AGENT_TOKEN!;
const COOLIFY_TOKEN = process.env.COOLIFY_TOKEN!;
const HDD = process.env.E2E_HDD!;

function pairingCode(): string {
  const json = JSON.stringify({
    agent_url: "http://vercelify-host.tail1234.ts.net:7420",
    agent_token: AGENT_TOKEN,
    coolify_url: "http://vercelify-host.tail1234.ts.net:8000",
    coolify_token: COOLIFY_TOKEN,
    hostname: "vercelify-host",
  });
  return "vfy1." + Buffer.from(json).toString("base64url");
}

async function coolifyApi<T = unknown>(method: string, path: string): Promise<T> {
  const r = await fetch(`${process.env.COOLIFY_URL}/api/v1${path}`, { method, headers: { Authorization: `Bearer ${COOLIFY_TOKEN}` } });
  return (await r.json()) as T;
}

/** The app's live page, fetched through its domain (Coolify's proxy). */
async function sitePage(name: string): Promise<string> {
  const apps = await coolifyApi<{ name: string; fqdn: string | null }[]>("GET", "/applications");
  const url = apps.find((a) => a.name === name)?.fqdn?.split(",")[0];
  if (!url) throw new Error(`${name} has no domain`);
  try {
    return await (await fetch(url, { signal: AbortSignal.timeout(5_000) })).text();
  } catch {
    return "";
  }
}

function pgContainerHealthy(): boolean {
  const out = execFileSync("docker", ["ps", "--filter", "label=coolify.resourceName=pg-main", "--format", "{{.Status}}"]).toString();
  return out.includes("(healthy)");
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
  await expect(page.getByTestId("host-status")).toContainText("via vercelify-host.tail1234.ts.net");
});

test("apps: list, deploy, deployments, build log, rollback, runtime logs", async ({ page }) => {
  await paired(page);
  const app = page.getByTestId("app-gitapp");
  await expect(app).toBeVisible();
  await expect(app.getByTestId("app-status")).toContainText("Running");
  await expect(page.getByTestId("app-whoami")).toBeVisible();
  await shot(page, "02-apps");

  await page.getByTestId("deploy-gitapp").click();
  await expect(page.getByText("Deployment of gitapp queued")).toBeVisible();

  await app.getByTestId("app-open").click();
  const rows = page.getByTestId("deployment-row");
  await expect(rows.first()).toBeVisible();
  // the deploy we just triggered finishes (Dockerfile build from git)
  await expect(rows.first()).toContainText("Ready", { timeout: 150_000 });
  await expect(page.getByTestId("rollback").first()).toBeVisible();
  await shot(page, "03-deployments");

  await rows.first().getByRole("button", { name: "Logs" }).click();
  await expect(page.getByTestId("build-log")).toContainText("Rolling update completed");
  await shot(page, "04-build-log");
  await page.keyboard.press("Escape");

  // Roll back to the previous commit and check the live site really serves it.
  const before = await sitePage("gitapp");
  const target = page.getByTestId("deployment-row").filter({ has: page.getByTestId("rollback") }).first();
  const sha = (await target.locator("code").first().textContent())!.trim();
  await target.getByTestId("rollback").click();
  await expect(page.getByText(`Rolling back gitapp to ${sha}`)).toBeVisible();
  await expect(rows.first()).toContainText("Current", { timeout: 150_000 });
  await expect(rows.first()).toContainText(sha);
  await expect(rows.first()).toContainText("Ready");
  await expect.poll(() => sitePage("gitapp"), { timeout: 60_000 }).not.toBe(before);
  await shot(page, "04b-rolled-back");

  await page.getByTestId("detail-tab-logs").click();
  await expect(page.locator("pre").first()).toContainText(/nginx|worker/);
  await page.getByTestId("detail-tab-previews").click();
  await expect(page.getByText(/No preview deployments|PR #/)).toBeVisible();
});

test("one-click database provisioning", async ({ page }) => {
  await paired(page);
  const name = `e2e-redis-${Date.now().toString(36)}`;
  await page.getByTestId("new-database").click();
  await page.getByTestId("db-kind").selectOption("redis");
  await page.getByTestId("db-name").fill(name);
  await page.getByTestId("db-create").click();
  await expect(page.getByText(`${name} is starting`)).toBeVisible();
  const row = page.getByTestId(`db-${name}`);
  await expect(row).toBeVisible();
  await expect(row).toContainText("Running", { timeout: 120_000 });
  await expect(row.getByRole("button", { name: "Copy URL" })).toBeVisible();
  await shot(page, "05-databases");
  // clean up through the API so repeated runs don't pile up databases
  const dbs = await coolifyApi<{ uuid: string; name: string }[]>("GET", "/databases");
  for (const d of dbs.filter((x) => x.name.startsWith("e2e-redis-"))) await coolifyApi("DELETE", `/databases/${d.uuid}`);
});

test("storage: tiers, assistant, move a live Postgres between SSD and HDD", async ({ page }) => {
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
  await shot(page, "06-storage");

  // Move pg-main (whichever tier it is on) and back, checking the data survives.
  const pg = page.locator('[data-testid^="volume-postgres-data-"]').first();
  await expect(pg).toBeVisible();
  for (let i = 0; i < 2; i++) {
    const button = pg.getByRole("button", { name: /Move to/ });
    const target = (await button.textContent())!.includes("HDD") ? "HDD" : "SSD";
    await button.click();
    await page.getByTestId("move-confirm").click();
    await expect(page.getByTestId("move-steps").locator("svg.text-ok")).toHaveCount(4, { timeout: 150_000 });
    await shot(page, `07-move-${target.toLowerCase()}`);
    await page.getByRole("button", { name: "Close", exact: true }).last().click();
    await expect(pg.getByText(target, { exact: true })).toBeVisible();
    // the restart has been queued; wait until Postgres is really up again
    await expect.poll(() => pgContainerHealthy(), { timeout: 120_000 }).toBe(true);
  }
  // Wait for Postgres to be healthy, then check the row written before any move.
  const container = execFileSync("docker", ["ps", "--filter", "label=coolify.resourceName=pg-main", "--format", "{{.Names}}"]).toString().trim();
  await expect
    .poll(
      () => {
        try {
          return execFileSync("docker", ["exec", container, "psql", "-U", "postgres", "-tAc", "select v from t"]).toString().trim();
        } catch {
          return "";
        }
      },
      { timeout: 90_000 },
    )
    .toBe("survives the move");
});

test("node health: resources, power, tailscale, containers, logs, services", async ({ page }) => {
  await paired(page);
  await page.getByTestId("nav-health").click();
  await expect(page.getByTestId("resources")).toContainText("CPU");
  await expect(page.getByTestId("power-source")).toBeVisible();
  await expect(page.getByTestId("ts-host")).toContainText("vercelify-host.tail1234.ts.net");
  await expect(page.getByTestId("ts-peer")).toHaveCount(2);
  const coolifyRow = page.getByTestId("container-coolify");
  await expect(coolifyRow).toBeVisible();
  await expect(coolifyRow).toContainText("%");
  await shot(page, "08-health");
  await coolifyRow.getByRole("button", { name: "Logs" }).click();
  await expect(page.getByTestId("container-logs")).not.toBeEmpty();
  await expect(page.getByTestId("container-logs")).not.toHaveText("Loading…");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("services")).toContainText("docker");
});

test("screencast: host receiver starts; desktop-only sender is reported clearly", async ({ page }) => {
  await paired(page);
  await page.getByTestId("nav-screencast").click();
  await expect(page.getByTestId("extend-card")).toContainText("mpv on the host");
  await page.getByTestId("source-test").click();
  await page.getByTestId("start-extend").click();
  // In the browser preview there is no local ffmpeg: the UI must say so and
  // roll back the receiver it started on the host.
  await expect(page.getByText("Available in the desktop app only")).toBeVisible();
  await expect(page.getByTestId("start-extend")).toBeVisible();
  await shot(page, "09-screencast");
});

test("settings: unpair returns to onboarding", async ({ page }) => {
  await paired(page);
  await page.getByTestId("nav-settings").click();
  await page.getByTestId("settings-computer").click();
  await page.getByTestId("unpair").click();
  await expect(page.getByTestId("pairing-code")).toBeVisible();
});
