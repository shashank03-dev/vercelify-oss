// Retakes the README's screenshot grid (apps, deployments, storage, health) in
// docs/screenshots from the demo backend, in the dark theme (NAME.png) and the
// light theme (NAME-light.png).
//   VITE_DEMO=1 npx vite --port 1431          (in cockpit/, leave it running)
//   node scripts/screenshots.mjs              (SCREENSHOT_URL overrides the address)
// Pages are captured at 1320x860 at 2x, with animations reduced.
import { chromium } from "@playwright/test";
import { fileURLToPath } from "node:url";

const BASE = process.env.SCREENSHOT_URL ?? "http://localhost:1431";
const OUT = fileURLToPath(new URL("../../docs/screenshots/", import.meta.url));

const browser = await chromium.launch();

for (const theme of ["dark", "light"]) {
  const context = await browser.newContext({
    viewport: { width: 1320, height: 860 },
    deviceScaleFactor: 2,
    colorScheme: theme,
    reducedMotion: "reduce",
  });
  // The app keeps its own theme choice (default dark), so set it before it loads.
  await context.addInitScript((t) => localStorage.setItem("vercelify.theme", t), theme);
  const page = await context.newPage();
  page.on("pageerror", (e) => console.error("page error:", e.message));
  const suffix = theme === "dark" ? "" : "-light";

  async function shot(name) {
    // Let polling fill in and transitions finish.
    await page.waitForLoadState("networkidle");
    await page.waitForTimeout(900);
    await page.mouse.move(1310, 850);
    await page.screenshot({ path: `${OUT}${name}${suffix}.png` });
    console.log(`docs/screenshots/${name}${suffix}.png`);
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
  await context.close();
}

await browser.close();
