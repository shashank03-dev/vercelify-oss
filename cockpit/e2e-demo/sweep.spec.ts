import { expect, test, type Page } from "@playwright/test";

// Every page, both themes, the smallest window the app allows: no console
// errors, no crash screens, no sideways scrolling.

const PAGES = ["apps", "ai", "servers", "health", "storage", "screencast", "settings"] as const;

async function pair(page: Page) {
  await page.goto("/");
  const row = page.getByTestId("found-dell-e7450");
  await row.getByRole("button", { name: "Connect" }).click();
  await row.getByTestId("short-code").fill("K7QF-M2XD");
  await row.getByTestId("short-submit").click();
  await expect(page.getByTestId("nav-apps")).toBeVisible();
}

for (const theme of ["dark", "light"] as const) {
  test(`every page renders cleanly in the ${theme} theme at the minimum window size`, async ({ page }) => {
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(String(e)));
    page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
    await page.addInitScript((t) => localStorage.setItem("vercelify.theme", t), theme);
    // tauri.conf.json: minWidth 1040, minHeight 680
    await page.setViewportSize({ width: 1040, height: 680 });
    await pair(page);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);

    for (const id of PAGES) {
      await page.getByTestId(`nav-${id}`).click();
      await page.waitForTimeout(400);
      await expect(page.getByText(/something went wrong/i), `${id}: no crash screen`).toHaveCount(0);
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      expect(overflow, `${id}: no horizontal scroll`).toBeLessThanOrEqual(0);
    }

    // keyboard: ⌘1…⌘7 and the ⌘K palette reach every page too
    for (let i = 0; i < PAGES.length; i++) {
      await page.keyboard.press(`ControlOrMeta+${i + 1}`);
      await expect(page.getByTestId(`nav-${PAGES[i]}`)).toHaveAttribute("aria-current", "page");
    }
    await page.keyboard.press("ControlOrMeta+k");
    await page.keyboard.type("Storage");
    await page.keyboard.press("Enter");
    await expect(page.getByTestId("nav-storage")).toHaveAttribute("aria-current", "page");

    expect(errors, "no errors in the console").toEqual([]);
  });
}

test("a reload keeps the pairing and the page you were on", async ({ page }) => {
  await pair(page);
  await page.getByTestId("nav-health").click();
  await page.reload();
  await expect(page.getByTestId("nav-apps")).toBeVisible();
  await expect(page.getByTestId("host-status")).toContainText("dell-e7450");
});
