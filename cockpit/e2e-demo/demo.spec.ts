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

/** Pair with a server found on the tailnet using its 8-character code. */
async function pairFound(page: Page, hostname: string, code = "K7QF-M2XD") {
  const row = page.getByTestId(`found-${hostname}`);
  await row.getByRole("button", { name: "Connect" }).click();
  await row.getByTestId("short-code").fill(code);
  await row.getByTestId("short-submit").click();
}

test("onboarding finds servers on the tailnet and pairs with a short code", async ({ page }) => {
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
});

/** Open the add-a-machine wizard for a machine that is already installed. */
async function addInstalled(page: Page, hostname: string, code = "ABCD-EFGH") {
  await page.getByTestId("host-status").click();
  await page.getByTestId("add-server").click();
  await page.getByTestId("already-installed").click();
  await pairFound(page, hostname, code);
}

test("several servers: add, switch, see them side by side, pool a worker", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");
  await expect(page.getByTestId("nav-apps")).toBeVisible();

  // add a worker with the wizard: role → command → code → pool → checks
  await page.getByTestId("host-status").click();
  await page.getByTestId("add-server").click();
  await page.getByTestId("kind-worker").click();
  await page.getByTestId("new-hostname").fill("Bad Name");
  await expect(page.getByTestId("wizard-next")).toBeDisabled();
  await page.getByTestId("new-hostname").fill("pi5-worker");
  await page.getByTestId("new-ai").check();
  await page.getByTestId("wizard-next").click();
  await expect(page.getByTestId("install-command")).toContainText("-s -- --worker --hostname pi5-worker --ai");
  await expect(page.getByTestId("pairing-code")).toHaveCount(0);
  await pairFound(page, "pi5-worker", "ABCD-EFGH");
  await expect(page.getByTestId("wizard-pool")).toBeVisible();
  await page.getByTestId("wizard-pool-run").click();
  await expect(page.getByText("pi5-worker joined dell-e7450's pool")).toBeVisible();
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
  await expect(page.getByTestId("server-dell-e7450").getByTestId("pool")).toContainText("pi5-worker");

  // pooling again is harmless
  await page.getByTestId("attach-pi5-worker").click();
  await page.getByTestId("attach-run").click();
  await expect(page.getByText("pi5-worker is already in dell-e7450's pool")).toBeVisible();
  await page.getByRole("dialog").getByRole("button", { name: "Close", exact: true }).first().click();

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
  // not on the tailnet yet…
  await expect(page.getByTestId("found-hp-elitebook")).toHaveCount(0);
  // …the installer finishes: it appears and its code field opens by itself
  const row = page.getByTestId("found-hp-elitebook");
  await expect(row).toContainText("code ready", { timeout: 20_000 });
  await expect(row.getByTestId("short-code")).toBeFocused();
  await row.getByTestId("short-code").fill("H7KM-2XQD");
  await row.getByTestId("short-submit").click();
  await expect(page.getByTestId("wizard-pool")).toBeVisible();
  await page.getByTestId("wizard-pool-run").click();
  await expect(page.getByText("hp-elitebook joined dell-e7450's pool")).toBeVisible();
  await page.getByTestId("wizard-done").click();
  await expect(page.getByTestId("server-hp-elitebook")).toBeVisible();
  await expect(page.getByTestId("server-dell-e7450").getByTestId("pool")).toContainText("hp-elitebook");

  // its free memory shows up when choosing where a new app runs
  await page.getByTestId("nav-apps").click();
  await page.getByTestId("new-app").click();
  await expect(page.getByTestId("new-app-server")).toContainText(/localhost · [\d.]+ GB free/);
  await expect(page.getByTestId("new-app-server")).toContainText("hp-elitebook ·");
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

  await expect(page.getByTestId("llm-snippet")).toContainText("dell-e7450.tail1234.ts.net:11434/v1");
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

test("apps: create from Git with auto-deploy, manage a branch watch", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");
  await expect(page.getByTestId("app-portfolio")).toContainText("auto");

  await page.getByTestId("new-app").click();
  await page.getByTestId("new-app-repo").fill("https://github.com/you/my-site");
  await expect(page.getByTestId("new-app-auto")).toBeChecked();
  await expect(page.getByTestId("new-app-server")).toContainText("pi5-worker");
  await page.getByTestId("new-app-branch").fill("a..b");
  await page.getByTestId("new-app-create").click();
  await expect(page.getByRole("alert")).toContainText("not a valid branch name");
  await page.getByTestId("new-app-branch").fill("main");
  await page.getByTestId("new-app-create").click();
  await expect(page.getByText("my-site is building — pushes to main deploy automatically")).toBeVisible();
  await expect(page.getByTestId("app-my-site")).toContainText("auto");

  await page.getByRole("button", { name: /notes-api/ }).first().click();
  await page.getByTestId("detail-tab-autodeploy").click();
  await expect(page.getByTestId("autodeploy")).toContainText("Watching main");
  await expect(page.getByTestId("autodeploy-repo")).toHaveValue("https://github.com/shashank03-dev/notes-api.git");
  await page.getByTestId("autodeploy-check").click();
  await expect(page.getByTestId("autodeploy")).toContainText("just now");
});

test("apps: import a .env file, see activity in the bell", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");
  await page.getByRole("button", { name: /notes-api/ }).first().click();

  await page.getByTestId("detail-tab-env").click();
  await expect(page.getByTestId("env-list")).toContainText("DATABASE_URL");
  await page.getByTestId("env-file").setInputFiles({
    name: ".env",
    mimeType: "text/plain",
    buffer: Buffer.from("# prod\nNODE_ENV=production\nSTRIPE_KEY='sk_live_1'\n"),
  });
  await expect(page.getByTestId("env-drop")).toContainText("2 variables · 1 new · 0 changed");
  await page.getByTestId("env-save").click();
  await expect(page.getByTestId("env-list")).toContainText("STRIPE_KEY");
  await expect(page.getByText("2 variables saved to notes-api")).toBeVisible();
  await page.keyboard.press("Escape");

  // the bell lists what needs attention and what happened this session
  await page.getByTestId("activity-button").click();
  await expect(page.getByTestId("activity-panel")).toContainText("discord-bot is exited");
  await expect(page.getByTestId("activity-panel")).toContainText("2 variables saved to notes-api");
});

test("server care: hardware, history, uptime, alerts, devices, backups, previews", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");

  // Health: what the machine is, 24 h of history recorded by the server, uptime of every app
  await page.getByTestId("nav-health").click();
  await expect(page.getByTestId("hardware")).toContainText("Memory limits");
  await expect(page.getByTestId("hardware")).toContainText("enforced");
  await expect(page.getByTestId("hardware")).toContainText("expires in");
  await expect(page.getByTestId("history")).toContainText("peak");
  await page.getByTestId("history").getByRole("radio", { name: "Temperature" }).click();
  await expect(page.getByTestId("history")).toContainText("°C");
  await expect(page.getByTestId("uptime")).toContainText("pi5-worker");

  // Settings: where alerts go, a test, paired devices, backups, recent changes
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("alerts")).toContainText("Sending to https://ntfy.sh/•••");
  await expect(page.getByTestId("alerts-list")).toContainText("dashboard: deploy of 9f2c1e0 failed");
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
  await expect(page.getByTestId("activity-panel")).toContainText("notes-api: rolled back to 4b1d2e7");
  await page.keyboard.press("Escape");

  // Apps: previews from pull requests and automatic rollback for a branch watch
  await page.getByTestId("nav-apps").click();
  await page.getByRole("button", { name: /portfolio/ }).first().click();
  await expect(page.getByTestId("rollback-kind").first()).toBeVisible();
  await page.getByTestId("detail-tab-autodeploy").click();
  await expect(page.getByTestId("autodeploy-previews")).toBeChecked();
  await expect(page.getByTestId("autodeploy-rollback")).toBeChecked();
  await expect(page.getByTestId("autodeploy-open-previews")).toContainText("Dark mode for the blog");
});

test("a Raspberry Pi worker shows its SD card and power supply", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "pi5-worker");
  await page.getByTestId("nav-health").click();
  await expect(page.getByTestId("hardware")).toContainText("Raspberry Pi 5 Model B");
  await expect(page.getByTestId("hardware")).toContainText("SD card");
  await expect(page.getByTestId("hardware")).toContainText("dipped since boot");
});

test("live: activity, sleeping apps, domains and power guard", async ({ page }) => {
  await page.goto("/");
  await pairFound(page, "dell-e7450");

  // An app that went to sleep is not a problem
  await page.getByTestId("nav-apps").click();
  await expect(page.getByTestId("app-blog").getByTestId("app-status")).toContainText("Sleeping");

  // Domains and sleep for one app
  await page.getByRole("button", { name: /portfolio/ }).first().click();
  await page.getByTestId("detail-tab-domains").click();
  await expect(page.getByTestId("domain-0")).toHaveValue("https://shashank.dev");
  await expect(page.getByTestId("app-domains")).toContainText("Routed through your Cloudflare tunnel");
  await page.getByRole("button", { name: "Add a domain" }).click();
  await page.getByTestId("domain-1").fill("not a domain");
  await expect(page.getByTestId("app-domains")).toContainText("not a domain is not a domain");
  await expect(page.getByTestId("domains-save")).toBeDisabled();
  await page.getByTestId("domain-1").fill("www.shashank.dev");
  await page.getByTestId("domains-save").click();
  await expect(page.getByText("Domains saved — redeploy portfolio to apply them")).toBeVisible();
  await page.getByTestId("sleep-toggle").click();
  await expect(page.getByText("portfolio sleeps after 30 idle minutes")).toBeVisible();
  await expect(page.getByTestId("sleep-toggle")).toBeChecked();
  await expect(page.getByTestId("sleep-state")).toContainText("awake");
  await page.keyboard.press("Escape");

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
  await page.getByRole("button", { name: /portfolio/ }).first().click();
  await page.getByTestId("detail-tab-project").click();
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
  await page.getByRole("button", { name: /notes-api/ }).first().click();
  await page.getByTestId("detail-tab-project").click();
  await expect(page.getByText("No vercel.json for this app yet")).toBeVisible();
  await page.getByTestId("project-file").setInputFiles({
    name: "vercel (1).json",
    mimeType: "application/json",
    buffer: Buffer.from(JSON.stringify({ redirects: [{ source: "/v1/:path*", destination: "/v2/:path*" }], crons: [{ path: "/api/cleanup", schedule: "0 3 * * *" }] })),
  });
  await expect(page.getByText("vercel.json applied to notes-api")).toBeVisible();
  await expect(page.getByTestId("project-summary")).toContainText("1 redirect · 1 cron job");
  await page.getByTestId("project-file").setInputFiles({ name: "package.json", mimeType: "application/json", buffer: Buffer.from("{}") });
  await expect(page.getByText("package.json: choose a vercel.json or vercelify.json")).toBeVisible();
  await page.getByTestId("project-remove").click();
  await expect(page.getByText("notes-api is served as Coolify configures it")).toBeVisible();
  await expect(page.getByText("No vercel.json for this app yet")).toBeVisible();
});
