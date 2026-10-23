import { defineConfig, devices } from "@playwright/test";
import { existsSync } from "node:fs";

// Use a preinstalled Chromium when one is provided (CI images, sandboxes).
const chromium = process.env.PW_CHROMIUM_PATH ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome";

export default defineConfig({
  testDir: "./e2e",
  timeout: 180_000,
  expect: { timeout: 20_000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"], ["html", { open: "never", outputFolder: "../playwright-report" }]],
  outputDir: "../test-results",
  use: {
    baseURL: process.env.COCKPIT_URL ?? "http://localhost:1421",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    colorScheme: "dark",
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1440, height: 900 },
        launchOptions: existsSync(chromium) ? { executablePath: chromium } : {},
      },
    },
  ],
});
