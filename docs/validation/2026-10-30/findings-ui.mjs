/** Run with the demo Vite server at :1431. Observes actual components with demo data.
 * Fixture overrides: one disk, private app domains, worn battery, local backup.
 * Instrumentation records demo calls and permits a real-shaped git-sources event.
 * No product sources, installed app settings or external accounts are modified.
 */
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { resolve, dirname } from "node:path";
import { writeFile } from "node:fs/promises";
import assert from "node:assert/strict";
const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../../..");
const require = createRequire(resolve(root, "cockpit/package.json"));
const { chromium, expect } = require("@playwright/test");
const browser = await chromium.launch();
const results = [];
const errors = [];

async function scenario(name, fn, pair = true) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  page.setDefaultTimeout(10_000);
  await page.addInitScript(() => {
    window.__reviewOpened = [];
    window.__reviewCalls = [];
    window.open = (url) => { window.__reviewOpened.push(String(url)); return null; };
  });
  await page.route("**/src/lib/demo.ts", async route => {
    const response = await route.fetch();
    let body = await response.text();
    const needle = "export async function demoCall(cmd, args) {";
    assert(body.includes(needle), "instrumentation signature changed");
    body = body.replace(needle, needle + "\nwindow.__reviewCalls.push({cmd,args});");
    assert(body.includes("const snap = systemBase();"));
    body = body.replace("const snap = systemBase();", "const snap = systemBase(); snap.disks = snap.disks.slice(0,1); snap.power.batteries[0].health_pct = 3.5;");
    const target = 'targets: ["/mnt/storage/backups/restic", "sftp:root@pi5-worker:/mnt/storage/backups/thinkpad-t470"]';
    assert(body.includes(target), "backup fixture changed");
    body = body.replace(target, 'targets: ["/var/backups/vercelify-restic"]');
    body += '\nexport function reviewEmit(topic) { emit("changed", {topic}); }\n';
    await route.fulfill({ response, body });
  });
  await page.route("**/src/lib/demo-platform.ts", async route => {
    const response = await route.fetch();
    let body = await response.text();
    assert(body.includes('const WILDCARD = "apps.shashank.dev";'));
    body = body.replace('const WILDCARD = "apps.shashank.dev";', 'const WILDCARD = "10.77.0.1.sslip.io";');
    body = body.replace('https: true,\n\t\tis_primary: primary', 'https: false,\n\t\tis_primary: primary');
    assert(body.includes('url: `https://${domains[0].host}`'));
    body = body.replace('url: `https://${domains[0].host}`', 'url: `http://${domains[0].host}`');
    await route.fulfill({ response, body });
  });
  try {
    await page.goto("http://localhost:1431");
    if (pair) {
      const row = page.getByTestId("found-optiplex-7050");
      await row.getByRole("button", { name: "Connect", exact: true }).click();
      await row.getByTestId("short-code").fill("K7QF-M2XD");
      await row.getByTestId("short-submit").click();
      await expect(page.getByTestId("nav-apps")).toBeVisible();
    }
    await fn(page);
  } catch (error) {
    errors.push({ scenario: name, error: String(error) });
  } finally {
    await page.unrouteAll({ behavior: "ignoreErrors" });
    await context.close();
  }
}
const record = (finding, observed, scope = "actual UI using controlled demo fixtures") => results.push({ finding, scope, observed });

await scenario("browser links, AI endpoints and app form", async page => {
  await page.getByTestId("papp-shop").getByRole("button", { name: /shop\.10\.77/ }).click();
  const opened = await page.evaluate(() => window.__reviewOpened);
  assert(opened.some(url => url.includes("10.77.0.1.sslip.io")));
  record("F2", { rawPrivateUrlPassedToBrowser: opened[0], note: "Browser transport forwarding is not provided by openExternal; actual Mac mesh reachability not exercised here." });
  await page.getByTestId("nav-ai").click();
  await expect(page.getByTestId("llm-snippet")).toBeVisible();
  const snippet = await page.getByTestId("llm-snippet").innerText();
  assert(snippet.includes("10.77.") && !/socks|proxy|localhost|127\.0\.0\.1/.test(snippet));
  record("F3", { exportsPrivateEndpoint: true, snippetContainsProxyOrLocalForwardSetup: false });
  await page.getByTestId("nav-apps").click();
  await page.getByTestId("new-app").click();
  const dialog = page.getByRole("dialog");
  const labels = await dialog.locator("label").allTextContents();
  assert.equal(await page.getByTestId("new-branch").inputValue(), "main");
  assert(!labels.some(text => /environment|variable|database/i.test(text)));
  record("F5", { initialBranch: "main", labels, initialVariablesOrDatabaseStep: false });
  const sourceChoices = await dialog.getByRole("radio").allTextContents();
  record("F9", { sourceChoices, noComposeOrEnvironmentControlInCreateDialog: !/compose|environment/i.test(labels.join(" ") + sourceChoices.join(" ")) });
  await page.getByTestId("new-repo").fill("https://github.com/you/validation-app");
  let escaped = false;
  for (let n = 0; n < 35; n++) {
    await page.keyboard.press("Tab");
    if (await page.evaluate(() => !document.activeElement?.closest('[role="dialog"]'))) { escaped = true; break; }
  }
  assert(escaped);
  record("F14", { keyboardFocusEscapedOpenDialog: escaped });
});

await scenario("GitHub callback with actual-shaped change event", async page => {
  await page.getByTestId("new-app").click();
  await page.getByTestId("new-repo").fill("https://github.com/you/private-validation");
  await page.getByTestId("github-app-open").click();
  await page.getByTestId("github-app-start").click();
  const backend = await page.evaluate(async () => {
    const { platformRoute } = await import("/src/lib/demo-platform.ts");
    const { reviewEmit } = await import("/src/lib/demo.ts");
    const source = platformRoute({ method: "POST", path: "/v1/platform/github-app/convert", body: { code: "completed-in-browser", state: "demo" } });
    reviewEmit("git-sources");
    return { sourceId: source.id, gitSources: platformRoute({ method: "GET", path: "/v1/platform/git-sources" }).length };
  });
  // Longer than the app-list's ordinary ten-second polling interval.
  await page.waitForTimeout(11_000);
  assert(await page.getByTestId("github-app").isVisible());
  assert.equal(await page.getByTestId("new-git-source").count(), 0);
  record("F4", { backend, gitSourcesChangeEventSent: true, observedAfterMs: 11_000, dialogStillOpen: true, newCredentialSelectorPresent: false }, "frontend callback completion simulated; no live GitHub account used");
});

await scenario("domains, backups and displayed battery wear", async page => {
  await page.getByTestId("papp-shop").getByRole("button").first().click();
  await page.getByTestId("tab-domains").click();
  await expect(page.getByTestId("domains-card")).toContainText("Point a DNS A record");
  record("F6", { domainInstructions: "Point a DNS A record at this server, add the name here, and HTTPS follows.", fixtureServerMode: "cloudflare" });
  await page.keyboard.press("Escape");
  await page.getByTestId("nav-settings").click();
  await expect(page.getByTestId("backups")).toContainText("/var/backups/vercelify-restic");
  const backup = await page.getByTestId("backups").innerText();
  assert(/Last backup/.test(backup));
  record("F7", { displaysSuccessfulLocalBackup: true, backupCardText: backup, note: "Same-disk default is tested separately using the real installer function." });
  await expect(page.getByTestId("domain")).toContainText("Cloudflare");
  await expect(page.getByTestId("domain")).toContainText("sudo vercelify domain");
  record("F15", { serverDomainChangeUsesCLIInstruction: true, note: "This is a workflow limitation, not evidence users cannot understand the instruction." });
  await page.getByTestId("nav-health").click();
  await expect(page.getByTestId("power")).toContainText("health 3.5%");
  record("F18", { healthPageAlreadyDisplaysBatteryHealth: "3.5%", correction: "The missing part is a doctor wear warning; health telemetry is already displayed." });
});

await scenario("absent storage choices", async page => {
  await page.getByTestId("new-db").click();
  await page.getByTestId("db-name").fill("validation-db");
  const dialog = page.getByRole("dialog");
  const hdd = dialog.getByRole("radio", { name: "HDD", exact: true });
  assert(await hdd.isEnabled());
  await hdd.click();
  assert(await page.getByTestId("db-create").isEnabled());
  record("F10", { singleDiskFixture: true, databaseHddOptionEnabled: true, databaseCreateEnabledAfterHddSelection: true });
  await page.keyboard.press("Escape");
  await page.getByTestId("nav-storage").click();
  await expect(page.getByTestId("tier-card-hdd")).toContainText("No disk mounted");
  await page.getByTestId("tier-hdd").click();
  assert(await page.getByTestId("create-volume").isEnabled());
  record("F10", { volumeAssistantConfirmsNoHdd: true, createFolderButtonStillEnabled: true });
  const buttonLabels = await page.locator("button").allTextContents();
  assert(!buttonLabels.some(label => /delete|purge/i.test(label)));
  record("F11", { storagePageHasDeleteOrPurgeButton: false });
});

await scenario("database restore starts without confirmation", async page => {
  await page.getByTestId("db-shop-db").click();
  await expect(page.getByTestId("db-backups")).toBeVisible();
  const restore = page.getByTestId("db-backups").getByRole("button", { name: "Restore", exact: true }).first();
  await expect(restore).toBeVisible();
  let nativeConfirmations = 0;
  page.on("dialog", async dialog => { nativeConfirmations++; await dialog.dismiss(); });
  const dialogsBefore = await page.getByRole("dialog").count();
  await restore.click();
  await expect.poll(() => page.evaluate(() => window.__reviewCalls.filter(call => call.cmd === "agent_request" && /\/restore$/.test(call.args.request?.path ?? "")).length)).toBe(1);
  const dialogsAfter = await page.getByRole("dialog").count();
  assert.equal(nativeConfirmations, 0);
  assert.equal(dialogsAfter, dialogsBefore);
  record("F8", { restoreRequestIssuedByFirstClick: true, newReviewDialog: false, nativeConfirmations });
  const hint = await page.getByText("Its data volume is kept", { exact: false }).innerText();
  assert(hint.includes("Storage page"));
  record("F11", { deletionHint: hint });
});

await scenario("remote onboarding wording and corrupt saved configuration", async page => {
  const text = await page.locator("main").innerText();
  assert(text.includes("Anywhere else, paste"));
  record("F13", { onboardingRequiresRemoteReadinessBeforeAnywhereElseClaim: false, note: "README separately includes the prerequisite; onboarding does not." });
  await page.evaluate(() => localStorage.setItem("vercelify.servers", "{invalid-json"));
  await page.reload();
  await expect(page.getByText("Welcome to Vercelify", { exact: true })).toBeVisible();
  const stillCorrupt = await page.evaluate(() => localStorage.getItem("vercelify.servers"));
  assert.equal(stillCorrupt, "{invalid-json");
  record("F17", { corruptBrowserConfigShowsOnboarding: true, originalCorruptValuePreservedUntilSave: true,
    correction: "Browser loader catches corruption; native read_config also defaults on read/parse failure. Shell rejection handler is not the primary cause." });
}, false);

await browser.close();
await writeFile(resolve(here, "ui-results.json"), JSON.stringify({ results, harnessErrors: errors }, null, 2) + "\n");
console.log(JSON.stringify({ results, harnessErrors: errors }, null, 2));
if (errors.length) process.exitCode = 1;
