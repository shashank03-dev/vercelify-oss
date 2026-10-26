import { defineConfig, devices } from "@playwright/test";
import { existsSync } from "node:fs";

// UI regression suite against the built-in demo backend (VITE_DEMO=1):
// no host or agent needed, so it runs in CI.
const chromium = process.env.PW_CHROMIUM_PATH ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome";

export default defineConfig({
  testDir: "./e2e-demo",
  timeout: 60_000,
  expect: { timeout: 10_000 },
  workers: 1,
  reporter: [["list"]],
  outputDir: "../test-results/demo",
  use: {
    baseURL: "http://localhost:1430",
    trace: "retain-on-failure",
    colorScheme: "dark",
  },
  webServer: {
    command: "npx vite --port 1430 --strictPort",
    env: { VITE_DEMO: "1" },
    url: "http://localhost:1430",
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
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
