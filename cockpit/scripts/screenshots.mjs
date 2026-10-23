// Retakes the README's screenshot grid (apps, deployments, storage, health) in
// docs/screenshots from the demo backend.
//   VITE_DEMO=1 npx vite --port 1431          (in cockpit/, leave it running)
//   node scripts/screenshots.mjs              (SCREENSHOT_URL overrides the address)
// Pages are captured at 1320x860 at 2x in the dark theme, with animations reduced.
import { chromium } from "@playwright/test";
import { fileURLToPath } from "node:url";

const BASE = process.env.SCREENSHOT_URL ?? "http://localhost:1431";
const OUT = fileURLToPath(new URL("../../docs/screenshots/", import.meta.url));

const browser = await chromium.launch();
const context = await browser.newContext({
  viewport: { width: 1320, height: 860 },
  deviceScaleFactor: 2,
  colorScheme: "dark",
  reducedMotion: "reduce",
});
const page = await context.newPage();
page.on("pageerror", (e) => console.error("page error:", e.message));

async function shot(name) {
  // Let polling fill in and transitions finish.
  await page.waitForLoadState("networkidle");
  await page.waitForTimeout(900);
  await page.mouse.move(1310, 850);
  await page.screenshot({ path: `${OUT}${name}.png` });
  console.log(`docs/screenshots/${name}.png`);
}

// Pair with the demo server using its short code.
await page.goto(BASE);
const row = page.getByTestId("found-dell-e7450");
await row.getByRole("button", { name: "Connect" }).click();
await row.getByTestId("short-code").fill("K7QF-M2XD");
await row.getByTestId("short-submit").click();
await page.getByTestId("nav-apps").waitFor();
// Keep the "Connected to …" toast out of the pictures.
await page.getByText("Connected to dell-e7450").waitFor({ state: "hidden", timeout: 20_000 });
await shot("apps");

await page.getByRole("row", { name: /notes-api/ }).getByRole("button", { name: "Deployments & logs" }).click();
await page.getByTestId("deployment-row").first().waitFor();
await shot("deployments");
await page.keyboard.press("Escape");

for (const tab of ["storage", "health"]) {
  await page.getByTestId(`nav-${tab}`).click();
  await shot(tab);
}

await browser.close();
