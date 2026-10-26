import { expect, test, type Page } from "@playwright/test";

// Every new flow, driven like a user against the demo backend.

const errors: string[] = [];

test.beforeEach(async ({ page }) => {
  errors.length = 0;
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
});

test.afterEach(() => {
  expect(errors, "no errors in the console").toEqual([]);
});

/** Pair with a server found on this computer's network using its 8-character code. */
async function pairFound(page: Page, hostname: string, code = "K7QF-M2XD") {
  const row = page.getByTestId(`found-${hostname}`);
  await row.getByRole("button", { name: "Connect" }).click();
  await row.getByTestId("short-code").fill(code);
  await row.getByTestId("short-submit").click();
}

test("onboarding finds servers on the network and pairs with a short code", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByText("Welcome to")).toBeVisible();
  // the long code stays available right away
  await expect(page.getByTestId("pairing-code")).toBeVisible();
  await expect(page.getByTestId("found-dell-e7450")).toContainText("code ready");
  await expect(page.getByTestId("found-pi5-worker")).toContainText("worker");

  // a wrong-length code can't be submitted
  const row = page.getByTestId("found-dell-e7450");
  await row.getByRole("button", { name: "Connect" }).click();
  await row.getByTestId("short-code").fill("K7QF");
  await expect(row.getByTestId("short-submit")).toBeDisabled();
  await row.getByTestId("short-code").fill("k7qf-m2xd");
  await expect(row.getByTestId("short-code")).toHaveValue("K7QF-M2XD");
  await row.getByTestId("short-submit").click();

  await expect(page.getByTestId("nav-apps")).toBeVisible();
  await expect(page.getByTestId("host-status")).toContainText("dell-e7450");
  await expect(page.getByTestId("platform-apps")).toBeVisible();
});

test("a server Coolify still runs says how to move it to Vercelify's engine", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "lenovo-x220");
  await expect(page.getByTestId("apps-migrate")).toContainText("sudo vercelify migrate --apply");
  await expect(page.getByText("sudo vercelify migrate --rollback")).toBeVisible();
  await page.getByTestId("nav-servers").click();
  await expect(page.getByTestId("checks-lenovo-x220")).toContainText("1 warning");
});

/** Open the add-a-machine wizard for a machine that is already installed. */
async function addInstalled(page: Page, hostname: string, code = "ABCD-EFGH") {
  await page.getByTestId("host-status").click();
  await page.getByTestId("add-server").click();
  await page.getByTestId("already-installed").click();
  await pairFound(page, hostname, code);
}

test("several servers: add, switch, see them side by side, add a worker", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");
  await expect(page.getByTestId("nav-apps")).toBeVisible();

  // add a worker with the wizard: role → command (with its invite) → code → checks
  await page.getByTestId("host-status").click();
  await page.getByTestId("add-server").click();
  await page.getByTestId("kind-worker").click();
  await page.getByTestId("new-hostname").fill("Bad Name");
  await expect(page.getByTestId("wizard-next")).toBeDisabled();
  await page.getByTestId("new-hostname").fill("pi5-worker");
  await page.getByTestId("new-ai").check();
  await page.getByTestId("wizard-next").click();
  // one main server: no choice to make
  await expect(page.getByTestId("worker-main")).toHaveCount(0);
  await expect(page.getByTestId("install-command")).toContainText("-s -- --worker --join vfyj1.demo-server-");
  await expect(page.getByTestId("install-command")).toContainText("--hostname pi5-worker --ai");
  await expect(page.getByTestId("pairing-code")).toHaveCount(0);
  await pairFound(page, "pi5-worker", "ABCD-EFGH");
  await expect(page.getByTestId("wizard-pool")).toContainText("It runs apps for dell-e7450");
  await expect(page.getByTestId("wizard-checks")).toContainText("1 warning");
  await expect(page.getByTestId("check-storage")).toContainText("single disk");
  await page.getByTestId("wizard-done").click();
  // adding a worker keeps the main server selected
  await expect(page.getByTestId("host-status")).toContainText("dell-e7450");

  // a worker has no Apps of its own
  await page.getByTestId("host-status").click();
  await expect(page.getByTestId("server-menu")).toBeVisible();
  await page.getByTestId("switch-pi5-worker").click();
  await expect(page.getByTestId("host-status")).toContainText("pi5-worker");
  await page.getByTestId("nav-apps").click();
  await expect(page.getByText("Deploy from your main server")).toBeVisible();
  await page.getByTestId("host-status").click();
  await page.getByTestId("switch-dell-e7450").click();
  await expect(page.getByTestId("host-status")).toContainText("dell-e7450");

  // the fleet view, with health checks per machine
  await page.getByTestId("nav-servers").click();
  await expect(page.getByTestId("server-dell-e7450")).toContainText("Online");
  await expect(page.getByTestId("server-dell-e7450")).toContainText("Current");
  await expect(page.getByTestId("checks-dell-e7450")).toContainText(/All \d+ checks pass/);
  await expect(page.getByTestId("server-pi5-worker")).toContainText("worker");
  // the workers its engine places apps on
  await expect(page.getByTestId("server-dell-e7450").getByTestId("pool")).toContainText("pi-5");

  // update every machine from the Mac
  await page.getByTestId("update-all").click();
  await expect(page.getByTestId("update-dell-e7450")).toContainText("Updating", { timeout: 15_000 });
  await expect(page.getByTestId("update-dell-e7450")).toContainText("Updated", { timeout: 30_000 });
  await expect(page.getByTestId("update-pi5-worker")).toContainText("Vercelify is ready", { timeout: 30_000 });

  // switch from a card, and from the command palette
  await page.getByTestId("switch-to-pi5-worker").click();
  await expect(page.getByTestId("host-status")).toContainText("pi5-worker");
  await page.keyboard.press("Control+k");
  await page.getByTestId("palette-input").fill("switch dell");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("host-status")).toContainText("dell-e7450");
});

test("add a machine: the wizard waits for a fresh install to come online", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");
  await page.getByTestId("nav-servers").click();
  await page.getByTestId("add-server-card").click();
  await expect(page.getByTestId("kind-worker")).toHaveAttribute("aria-checked", "true");
  await page.getByTestId("wizard-next").click();
  await expect(page.getByTestId("install-command")).toContainText("--worker");
  // not on the network yet…
  await expect(page.getByTestId("found-hp-elitebook")).toHaveCount(0);
  // …the installer finishes: it appears and its code field opens by itself
  const row = page.getByTestId("found-hp-elitebook");
  await expect(row).toContainText("code ready", { timeout: 20_000 });
  await expect(row.getByTestId("short-code")).toBeFocused();
  await row.getByTestId("short-code").fill("H7KM-2XQD");
  await row.getByTestId("short-submit").click();
  await expect(page.getByTestId("wizard-pool")).toContainText("It runs apps for dell-e7450");
  await page.getByTestId("wizard-done").click();
  await expect(page.getByTestId("server-hp-elitebook")).toBeVisible();
  await expect(page.getByTestId("server-hp-elitebook")).toContainText("Worker");
});

test("AI models: chat streams, models download, snippets for the Mac", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");
  await page.getByTestId("nav-ai").click();

  await expect(page.getByTestId("llm-status")).toContainText("Ollama 0.12.3");
  await expect(page.getByTestId("model-llama3.2:3b")).toContainText("in memory");

  await page.getByTestId("chat-input").fill("hello");
  await page.getByTestId("chat-send").click();
  await expect(page.getByTestId("chat-log")).toContainText("on your old laptop");
  await expect(page.getByTestId("chat-log")).toContainText("tokens/s");

  await page.getByTestId("pull-gemma3:1b").click();
  await expect(page.getByTestId("llm-catalog")).toContainText("pulling");
  await expect(page.getByTestId("model-gemma3:1b")).toBeVisible({ timeout: 30_000 });

  await expect(page.getByTestId("llm-snippet")).toContainText("10.77.0.1:11434/v1");
  await page.getByRole("radio", { name: "Your apps" }).click();
  await expect(page.getByTestId("llm-snippet")).toContainText("http://vercelify-ollama:11434");

  // a worker without Ollama offers to install it
  await addInstalled(page, "pi5-worker");
  await page.getByTestId("wizard-done").click();
  await page.getByTestId("host-status").click();
  await page.getByTestId("switch-pi5-worker").click();
  await page.getByTestId("nav-ai").click();
  await expect(page.getByTestId("llm-install")).toBeVisible();
  await expect(page.getByTestId("llm-install-button")).toBeEnabled();
});

test("apps: a failed build is explained by Gemma 4 on the server", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");
  await page.getByTestId("papp-api").getByRole("button", { name: /api/ }).first().click();
  await expect(page.getByTestId("deploy-log")).toContainText("undefined: handlers.Notes");

  await page.getByTestId("explain-failure").click();
  const answer = page.getByTestId("explain-answer");
  await expect(answer).toContainText("Demo mode");
  await expect(answer).toContainText("Cause:");
  await expect(answer).toContainText("Fix:");
  await expect(answer).toContainText("Check:");
  await expect(page.getByTestId("explain-meta")).toContainText("gemma4:e2b on dell-e7450");
  await expect(page.getByTestId("explain-meta")).toContainText("tokens/s");
});

test("server care: hardware, history, uptime, alerts, devices, backups, previews", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");

  // Health: what the machine is, 24 h of history recorded by the server, uptime of every app
  await page.getByTestId("nav-health").click();
  await expect(page.getByTestId("hardware")).toContainText("Memory limits");
  await expect(page.getByTestId("hardware")).toContainText("enforced");
  await expect(page.getByTestId("history")).toContainText("peak");
  await page.getByTestId("history").getByRole("radio", { name: "Temperature" }).click();
  await expect(page.getByTestId("history")).toContainText("°C");
  await expect(page.getByTestId("uptime")).toContainText("pi5-worker");

  // Settings: where alerts go, a test, paired devices, backups, recent changes
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("alerts")).toContainText("Sending to https://ntfy.sh/•••");
  await expect(page.getByTestId("alerts-list")).toContainText("shop: deploy of 9f2c1e0 failed");
  await page.getByTestId("alerts-test").click();
  await expect(page.getByText("Test notification sent")).toBeVisible();
  await expect(page.getByTestId("devices")).toContainText("this app");
  await page.getByTestId("device-b71d08aa").getByRole("button", { name: /Revoke/ }).click();
  await expect(page.getByText("iPad (status only) can no longer connect")).toBeVisible();
  await expect(page.getByTestId("devices")).not.toContainText("iPad");
  await expect(page.getByTestId("backups")).toContainText("sftp:root@pi5-worker");
  await page.getByTestId("backup-run").click();
  await expect(page.getByText("Backup of dell-e7450 started")).toBeVisible();
  await expect(page.getByTestId("audit")).toContainText("/v1/update");

  // the bell shows what the server reported
  await page.getByTestId("activity-button").click();
  await expect(page.getByTestId("activity-panel")).toContainText("api: rolled back to 4b1d2e7");
  await page.keyboard.press("Escape");
});

test("a Raspberry Pi worker shows its SD card and power supply", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "pi5-worker");
  await page.getByTestId("nav-health").click();
  await expect(page.getByTestId("hardware")).toContainText("Raspberry Pi 5 Model B");
  await expect(page.getByTestId("hardware")).toContainText("SD card");
  await expect(page.getByTestId("hardware")).toContainText("dipped since boot");
});

test("live: activity, sleeping apps, the domain and power guard", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");

  // An app that went to sleep is not a problem
  await page.getByTestId("nav-apps").click();
  await expect(page.getByTestId("status-wiki")).toContainText("Asleep");

  // A backup started from Settings shows up live in the activity list
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("domain")).toContainText("Cloudflare tunnel");
  await expect(page.getByTestId("power-guard")).toContainText("AC power");
  await page.getByTestId("pg-thermal_pause_c").fill("85");
  await page.getByTestId("pg-save").click();
  await expect(page.getByText("Power guard updated")).toBeVisible();
  await expect(page.getByRole("button", { name: "Use this machine's defaults" })).toBeVisible();
  await page.getByTestId("backup-run").click();
  await page.getByTestId("nav-health").click();
  await expect(page.getByTestId("activity")).toContainText("Live from the server");
  await expect(page.getByTestId("job-backup").first()).toContainText(/running|%/);
  await expect(page.getByTestId("job-backup").first()).toContainText("done", { timeout: 10_000 });
  await expect(page.getByTestId("activity")).toContainText("went to sleep");
});

test("vercel.json: rules, warnings, cron jobs, upload and removal", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");
  await page.getByTestId("nav-apps").click();

  // Read from the repository at the deployed commit
  await page.getByTestId("papp-shop").getByRole("button", { name: /shop/ }).first().click();
  await page.getByTestId("tab-project").click();
  await expect(page.getByTestId("project-state")).toContainText("live in the proxy");
  await expect(page.getByTestId("project-summary")).toContainText("4 redirects · 1 rewrite · 2 header rules · 2 cron jobs · clean URLs");
  await expect(page.getByTestId("project-warnings")).toContainText("`functions` is not used here");
  await expect(page.getByTestId("app-project-previews")).toContainText("#42");

  // A failing cron job shows as such; run it by hand
  await expect(page.getByTestId("cron-state-1")).toContainText("answered HTTP 500");
  await page.getByTestId("cron-run-1").click();
  await expect(page.getByText("Running /api/cron/digest")).toBeVisible();
  await expect(page.getByTestId("cron-state-1")).toContainText("HTTP 200", { timeout: 15_000 });
  await page.keyboard.press("Escape");

  // Upload a vercel.json for an app on another Git host
  await page.getByTestId("papp-api").getByRole("button", { name: /api/ }).first().click();
  await page.getByTestId("tab-project").click();
  await expect(page.getByText("No vercel.json for this app yet")).toBeVisible();
  await page.getByTestId("project-file").setInputFiles({
    name: "vercel (1).json",
    mimeType: "application/json",
    buffer: Buffer.from(JSON.stringify({ redirects: [{ source: "/v1/:path*", destination: "/v2/:path*" }], crons: [{ path: "/api/cleanup", schedule: "0 3 * * *" }] })),
  });
  await expect(page.getByText("vercel.json applied to api")).toBeVisible();
  await expect(page.getByTestId("project-summary")).toContainText("1 redirect · 1 cron job");
  await page.getByTestId("project-file").setInputFiles({ name: "package.json", mimeType: "application/json", buffer: Buffer.from("{}") });
  await expect(page.getByText("package.json: choose a vercel.json or vercelify.json")).toBeVisible();
  await page.getByTestId("project-remove").click();
  await expect(page.getByText("api is served without its rules")).toBeVisible();
  await expect(page.getByText("No vercel.json for this app yet")).toBeVisible();
});

test("platform engine: deploy with a live log, roll back, variables, domains, databases", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "optiplex-7050");
  await expect(page.getByTestId("host-status")).toContainText("optiplex-7050");
  await expect(page.getByTestId("platform-apps")).toBeVisible();
  await expect(page.getByTestId("status-shop")).toContainText("Live");
  await expect(page.getByTestId("status-api")).toContainText("last deploy failed");

  // a new app from Git: created, built with a streamed log, then live
  await page.getByTestId("new-app").click();
  await page.getByTestId("new-repo").fill("https://github.com/you/My-Site.git");
  await expect(page.getByTestId("new-name")).toHaveValue("my-site");
  await page.getByTestId("create-app").click();
  await expect(page.getByTestId("app-detail")).toContainText("my-site");
  await expect(page.getByTestId("deploy-log")).toContainText("Detected Next.js");
  await expect(page.getByTestId("deploy-log")).toContainText("Deployment is live", { timeout: 15_000 });
  await expect(page.getByTestId("deployment-1")).toContainText("Live");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("status-my-site")).toContainText("Live");

  // a failed build, then a rollback to an earlier version
  await page.getByTestId("papp-api").getByRole("button", { name: /api/ }).first().click();
  await expect(page.getByTestId("deploy-log")).toContainText("undefined: handlers.Notes");
  await page.getByTestId("rollback-1").click();
  await expect(page.getByTestId("deployment-4")).toContainText("Live", { timeout: 15_000 });

  // variables and an imported .env
  await page.getByTestId("tab-env").click();
  await page.getByTestId("env-key").fill("api_url");
  await expect(page.getByTestId("env-key")).toHaveValue("API_URL");
  await page.getByTestId("env-value").fill("https://api.example.com");
  await page.getByTestId("env-add").click();
  await expect(page.getByTestId("env-API_URL")).toBeVisible();
  await page.getByTestId("env-import").click();
  await page.getByTestId("env-import-text").fill("NODE_ENV=production\nexport FLAG=1\n");
  await page.getByTestId("env-import-run").click();
  await expect(page.getByTestId("env-FLAG")).toBeVisible();

  // a custom domain gets its certificate
  await page.getByTestId("tab-domains").click();
  await page.getByTestId("domain-input").fill("notes.example.com");
  await page.getByTestId("domain-add").click();
  await expect(page.getByTestId("domain-notes.example.com")).toContainText("HTTPS", { timeout: 15_000 });

  // a deploy hook is shown once
  await page.getByTestId("tab-settings").click();
  await page.getByTestId("hook-create").click();
  await expect(page.getByTestId("hook-url")).toContainText("/v1/hooks/deploy/vfyh_");
  await page.keyboard.press("Escape");

  // a database connected to an app, backed up
  await page.getByTestId("new-db").click();
  await page.getByTestId("db-name").fill("notes-db");
  await page.getByTestId("db-link").selectOption({ label: "api" });
  await page.getByTestId("db-create").click();
  await expect(page.getByTestId("db-detail")).toContainText("notes-db");
  await expect(page.getByTestId("db-url")).toContainText("postgres://notes-db:••••@vfy-db-notes-db:5432/notes-db");
  await page.getByTestId("db-backup").click();
  await expect(page.getByTestId("db-backups")).toContainText("just now", { timeout: 10_000 });
  await page.keyboard.press("Escape");

  // the Storage page lists the platform's volumes and moves them
  await page.getByTestId("nav-storage").click();
  await expect(page.getByText("SSD tier")).toBeVisible();
  await expect(page.getByTestId("platform-volumes")).toContainText("umami-data");
  await page.getByTestId("pmove-shop-db-data").click();
  await page.getByTestId("pmove-run").click();
  await expect(page.getByText("shop-db is on the HDD now")).toBeVisible({ timeout: 10_000 });
  await expect(page.getByTestId("pvolume-shop-db-data")).toContainText("HDD");
  await page.getByTestId("nav-servers").click();
  await expect(page.getByText("optiplex-7050").first()).toBeVisible();
});

test("platform engine: live output, a terminal, restart, services and a database console", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "optiplex-7050");
  await page.getByTestId("papp-shop").getByRole("button", { name: /shop/ }).first().click();

  // the app's own output, live
  await page.getByTestId("tab-logs").click();
  await expect(page.getByTestId("runtime-log")).toContainText("Ready in 412ms");
  await expect(page.getByTestId("runtime-log")).toContainText("/checkout", { timeout: 10_000 });
  await page.getByTestId("logs-filter").fill("ready");
  await expect(page.getByTestId("runtime-log")).not.toContainText("GET /cart");

  // a shell in the container
  await page.getByTestId("tab-terminal").click();
  const term = page.getByTestId("terminal");
  await expect(term).toHaveAttribute("data-state", "open");
  await expect(term).toContainText("/app $");
  await term.click();
  await page.keyboard.type("echo hello from the box");
  await page.keyboard.press("Enter");
  await expect(term).toContainText("hello from the box", { timeout: 10_000 });
  await page.keyboard.type("exit");
  await page.keyboard.press("Enter");
  await expect(term).toHaveAttribute("data-state", "ended");

  // restart, and a volume for the next deploy
  await page.getByTestId("app-restart").click();
  await expect(page.getByText("Restarted shop")).toBeVisible();
  await page.getByTestId("tab-settings").click();
  await page.getByTestId("volume-name").fill("uploads");
  await page.getByTestId("volume-path").fill("/app/uploads");
  await page.getByTestId("volume-add").click();
  await expect(page.getByTestId("volume-uploads")).toContainText("/app/uploads");
  await page.keyboard.press("Escape");

  // a service from the catalogue
  await page.getByTestId("new-app").click();
  await page.getByTestId("kind-service").click();
  await page.getByTestId("template-uptime-kuma").click();
  await expect(page.getByTestId("new-name")).toHaveValue("uptime-kuma");
  await page.getByTestId("new-name").fill("status");
  await page.getByTestId("create-app").click();
  await expect(page.getByTestId("app-detail")).toContainText("status");
  await expect(page.getByTestId("deploy-log")).toContainText("Deployment is live", { timeout: 15_000 });
  await page.keyboard.press("Escape");

  // the database's own console
  await page.getByTestId("db-shop-db").click();
  await page.getByTestId("db-console").click();
  const psql = page.getByTestId("db-terminal");
  await expect(psql).toContainText("shop-db=>");
  await psql.click();
  await page.keyboard.type("select 1;");
  await page.keyboard.press("Enter");
  await expect(psql).toContainText("(1 row)", { timeout: 10_000 });
});

test("Vercelify network: pair on the LAN, see each path, invite a server, revoke a device", async ({ page }) => {
  await page.goto("/");
  // Found by mDNS: no address to type.
  const lan = page.getByTestId("lan-hp-elitebook");
  await expect(lan).toContainText("code ready");
  await lan.getByRole("button", { name: "Connect" }).click();
  await lan.getByTestId("short-code").fill("k7qf-m2xd");
  await lan.getByTestId("short-submit").click();
  await expect(page.getByTestId("nav-apps")).toBeVisible();

  await page.getByTestId("nav-servers").click();
  const net = page.getByTestId("mesh-network");
  await expect(net.getByTestId("mesh-local")).toContainText("This computer: macbook-air");
  await expect(net.getByTestId("mesh-local")).toContainText("app mode");
  // How this Mac reaches each node: the LAN, or the relay for the Pi behind CGNAT.
  await expect(net.getByTestId("mesh-node-thinkpad-t470")).toContainText("coordinator");
  await expect(net.getByTestId("mesh-node-thinkpad-t470")).toContainText("LAN");
  await expect(net.getByTestId("mesh-node-pi5-worker")).toContainText("relay");
  await expect(net.getByTestId("mesh-node-pi5-worker")).toContainText("on the relay");
  await expect(net.getByTestId("mesh-node-macbook-air")).toContainText("this computer");
  await expect(net.getByTestId("mesh-revoke-thinkpad-t470")).toHaveCount(0);

  // Invite a server: a join command to run on it.
  await net.getByTestId("mesh-invite-open").click();
  await page.getByTestId("mesh-invite-create").click();
  await expect(page.getByTestId("mesh-invite-command")).toContainText("sudo vercelify mesh join vfyj1.");
  await page.getByRole("dialog").getByRole("button", { name: "Close", exact: true }).first().click();

  // Revoke the old iPad, after a confirmation.
  await net.getByTestId("mesh-revoke-old-ipad").click();
  await page.getByTestId("mesh-revoke-yes").click();
  await expect(page.getByText("old-ipad is off the network")).toBeVisible();
  await expect(net.getByTestId("mesh-node-old-ipad")).toContainText("revoked");
  await expect(net.getByTestId("mesh-revoke-old-ipad")).toHaveCount(0);
});

test("platform engine: apps across servers — where they run, replicas, drain a worker", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "optiplex-7050");
  // api runs twice: on the Pi and on the main server.
  await page.getByTestId("papp-api").getByRole("button", { name: /api/ }).first().click();
  await page.getByTestId("tab-settings").click();
  const placement = page.getByTestId("placement-card");
  await expect(placement.getByTestId("instances")).toContainText("pi-5");
  await expect(placement.getByTestId("instances")).toContainText("optiplex-7050");
  await expect(placement.getByTestId("placement-node")).toHaveValue("node_demopi5000000000");
  await expect(placement.getByTestId("placement-failover")).toBeChecked();
  await placement.getByTestId("placement-replicas").fill("3");
  await placement.getByTestId("placement-save").click();
  await expect(page.getByText("Saved: the next deployment uses it")).toBeVisible();
  // until the next deploy, what runs stays as it is
  await expect(placement.getByTestId("instances").locator("li")).toHaveCount(2);
  await page.keyboard.press("Escape");

  // the fleet on the Servers page
  await page.getByTestId("nav-servers").click();
  const fleet = page.getByTestId("platform-servers");
  await expect(fleet.getByTestId("platform-node-optiplex-7050")).toContainText("main");
  await expect(fleet.getByTestId("platform-node-pi-5")).toContainText("aarch64");
  await expect(fleet.getByTestId("platform-node-pi-5")).toContainText("online");
  // a warm standby for the main server
  await fleet.getByTestId("platform-standby-node").selectOption({ label: "pi-5" });
  await expect(fleet.getByTestId("platform-standby-state")).toContainText("sudo vercelify promote on pi-5");
  await fleet.getByTestId("platform-drain-pi-5").click();
  await expect(fleet.getByTestId("platform-node-pi-5")).toContainText("drained");
  // a worker with instances can't be removed
  await fleet.getByRole("button", { name: "Remove pi-5" }).click();
  await page.getByTestId("platform-remove-yes").click();
  await expect(page.getByText(/still runs/)).toBeVisible();
});

test("platform engine: a GitHub App for private repositories, and a webhook", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "optiplex-7050");
  await page.getByTestId("new-app").click();
  await page.getByTestId("new-repo").fill("https://github.com/you/private-site");
  await page.getByTestId("github-app-open").click();
  const gh = page.getByTestId("github-app");
  await gh.getByTestId("github-app-start").click();
  await expect(gh).toContainText("GitHub opened in your browser");
  await expect(gh.getByTestId("github-app-finish")).toBeDisabled();
  await gh.getByTestId("github-app-address").fill("http://192.168.1.20:7443/v1/github-app/callback?code=abc&state=demo");
  await gh.getByTestId("github-app-finish").click();
  await expect(page.getByText(/is ready: install it on your repositories/)).toBeVisible();
  await expect(page.getByTestId("new-git-source")).toHaveValue(/git_/);
  await expect(page.getByTestId("github-app-open")).toHaveCount(0);
  await page.getByTestId("create-app").click();
  await expect(page.getByTestId("app-detail")).toContainText("private-site");

  // pushes deploy at once with a webhook
  await page.getByTestId("tab-settings").click();
  await page.getByTestId("autodeploy-toggle").click();
  await page.getByTestId("webhook-open").click();
  const hook = page.getByTestId("webhook-setup");
  await expect(hook).toContainText("It has a webhook already");
  await hook.getByTestId("webhook-create").click();
  await expect(hook.getByTestId("webhook-url")).toContainText("/v1/webhooks/github/git_");
  await expect(hook.getByTestId("webhook-secret")).toHaveText(/\S{30,}/);
});
