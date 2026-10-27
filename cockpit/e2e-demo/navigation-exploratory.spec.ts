import { expect, test } from "@playwright/test";

for (const theme of ["dark", "light"] as const) {
  test(`all pages and keyboard navigation work in ${theme} mode`, async ({ page }) => {
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await page.addInitScript((value) => localStorage.setItem("vercelify.theme", value), theme);
    await page.goto("/");
    const server = page.getByTestId("found-dell-e7450");
    await server.getByRole("button", { name: "Connect" }).click();
    await server.getByTestId("short-code").fill("K7QF-M2XD");
    await server.getByTestId("short-submit").click();
    await expect(page.getByTestId("nav-apps")).toBeVisible();
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);

    for (const [index, id] of ["apps", "ai", "servers", "health", "storage", "screencast", "settings"].entries()) {
      await page.keyboard.press(`Control+${index + 1}`);
      await expect(page.getByTestId(`nav-${id}`)).toHaveAttribute("aria-current", "page");
      await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    }

    await page.keyboard.press("Control+k");
    await page.getByTestId("palette-input").fill("AI models");
    await page.keyboard.press("Enter");
    await expect(page.getByTestId("nav-ai")).toHaveAttribute("aria-current", "page");
    expect(errors).toEqual([]);
  });
}
