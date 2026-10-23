// Desktop end-to-end: drives the real Tauri binary through tauri-driver
// (WebKitWebDriver) so the Rust command layer is exercised for real:
// settings persistence, HTTP to the agent and Coolify, local tools, and the
// ffmpeg sender streaming into the agent's mpv receiver.
//
// Started by e2e-desktop/run-desktop.sh.

import assert from "node:assert/strict";
import { statSync, existsSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { after, afterEach, before, test } from "node:test";
import { writeFileSync, mkdirSync } from "node:fs";
import { Builder, By, until } from "selenium-webdriver";

const APP = process.env.COCKPIT_BIN;
const AGENT = process.env.AGENT_URL;
const AGENT_TOKEN = process.env.AGENT_TOKEN;
const COOLIFY = process.env.COOLIFY_URL;
const COOLIFY_TOKEN = process.env.COOLIFY_TOKEN;
const SETTINGS = `${process.env.XDG_CONFIG_HOME}/dev.vercelify.cockpit/settings.json`;

let driver;

const byTest = (id) => By.css(`[data-testid="${id}"]`);
const find = (id, ms = 20_000) => driver.wait(until.elementLocated(byTest(id)), ms);
// WebKitWebDriver's getText() is empty under `user-select: none`; read textContent.
const text = async (id) => driver.executeScript("return arguments[0].textContent", await find(id));
async function waitText(id, re, ms = 30_000) {
  await driver.wait(async () => re.test(await text(id).catch(() => "")), ms, `${id} never matched ${re}`);
}
async function agentGet(path) {
  const r = await fetch(AGENT + path, { headers: { Authorization: `Bearer ${AGENT_TOKEN}` } });
  return r.json();
}
async function startApp() {
  driver = await new Builder()
    .usingServer("http://127.0.0.1:4444/")
    .withCapabilities({ browserName: "wry", "tauri:options": { application: APP } })
    .build();
}

before(startApp);
afterEach(async (t) => {
  if (!driver) return;
  mkdirSync("../e2e-logs/desktop", { recursive: true });
  const name = t.name.replace(/[^a-z0-9]+/gi, "-").slice(0, 60);
  writeFileSync(`../e2e-logs/desktop/${name}.png`, Buffer.from(await driver.takeScreenshot(), "base64"));
});
after(async () => driver && (await driver.quit()));

test("pairs through Rust and persists settings with 0600 permissions", async () => {
  const code =
    "vfy1." +
    Buffer.from(
      JSON.stringify({ agent_url: AGENT, agent_token: AGENT_TOKEN, coolify_url: COOLIFY, coolify_token: COOLIFY_TOKEN, hostname: "desk" }),
    ).toString("base64url");
  await (await find("pairing-code")).sendKeys(code);
  await (await find("pair-submit")).click();
  await find("nav-apps");
  assert.ok(existsSync(SETTINGS), "settings.json written");
  assert.equal(statSync(SETTINGS).mode & 0o777, 0o600);
});

test("Coolify apps load via the Rust HTTP proxy", async () => {
  await (await find("nav-apps")).click();
  await find("app-gitapp", 30_000);
  await waitText("app-gitapp", /Running/);
});

test("local tools are detected on this machine", async () => {
  await (await find("nav-settings")).click();
  await (await find("settings-computer")).click();
  await driver.wait(until.elementLocated(By.xpath("//*[contains(text(),'/ffmpeg')]")), 10_000);
});

test("Screencast streams a test pattern into the host's mpv receiver", async () => {
  await (await find("nav-screencast")).click();
  await (await find("source-test")).click();
  await (await find("start-extend")).click();
  // both ends report running
  await find("stop-extend", 30_000);
  await waitText("extend-status", /^\s*Live\s*$/, 30_000);
  // mpv decoded real H.264 frames from ffmpeg (VO line from the receiver's stderr)
  await driver.wait(
    async () => (await agentGet("/v1/display")).receiver.last_output.some((l) => /VO: \[null\] \d+x\d+/.test(l)),
    30_000,
    "receiver never decoded video",
  );
  await (await find("stop-extend")).click();
  await find("start-extend", 30_000);
  assert.equal((await agentGet("/v1/display")).receiver.running, false);
});

const ffmpegs = () => {
  try {
    return execFileSync("pgrep", ["-x", "ffmpeg"]).toString().trim().split("\n").filter(Boolean).length;
  } catch {
    return 0;
  }
};

test("a force-quit app does not leave ffmpeg streaming", async () => {
  await (await find("start-extend")).click();
  await find("stop-extend", 30_000);
  assert.ok(ffmpegs() >= 1, "sender running");
  await driver.quit(); // tauri-driver kills the app: no exit hooks run
  driver = undefined;
  const deadline = Date.now() + 10_000;
  while (ffmpegs() > 0 && Date.now() < deadline) await new Promise((r) => setTimeout(r, 250));
  assert.equal(ffmpegs(), 0, "orphaned ffmpeg is still running");
  await fetch(AGENT + "/v1/display/receiver/stop", { method: "POST", headers: { Authorization: `Bearer ${AGENT_TOKEN}`, "Content-Type": "application/json" }, body: "{}" });
});

test("settings survive an app restart (no onboarding)", async () => {
  if (driver) await driver.quit();
  await startApp();
  await find("nav-apps", 30_000);
  await waitText("host-status", /desk|via/);
});
